//! #2242 (pentagi tekrar dedektörü fikri, Rust'ta yeniden yazım): aynı aracı aynı normalize
//! girdiyle art arda çağırma döngüsünü deterministik yakalayan bekçi. Bellek içi, ajan başına
//! durum ([`crate::progress`] gibi); bir core restart'ı çalışan işleri zaten kapatır.
//!
//! ADIM 0 sonucu: Claude Code hook dokümanına göre `PreToolUse` ve `PostToolUse` cevaplarındaki
//! `hookSpecificOutput.additionalContext` modele ulaşır ("String added to Claude's context
//! alongside the tool result"). Repodaki kanıt: `worker_notes::context_texts`, PostToolUse ile
//! verilen bağlamın transcript'te `hook_additional_context` attachment'ı olarak geri geldiğini
//! teslim makbuzu sayar. Bu yüzden uyarı, izin kararına dokunmadan PostToolUse cevabına eklenir.
//! İlk sürüm yalnız uyarı + lider notu; Deny yok.
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Aynı normalize çağrı art arda bu kez olunca ajana uyarı gider.
pub(crate) const REPEAT_WARN: u32 = 3;
/// Aynı normalize çağrı art arda bu kez olunca lidere "takıldı" notu düşer.
pub(crate) const STUCK_CONSECUTIVE: u32 = 5;
/// Aynı normalize çağrı bir işte (düzeltme Edit/Write'ı toplamı sıfırlar) toplam bu kez
/// olunca lidere "takıldı" notu düşer.
pub(crate) const STUCK_TOTAL: u32 = 10;
/// Aynı çağrı art arda bu keze ulaşınca uyarı, kısmi sonucu `report` ile teslimi de söyler.
pub(crate) const REPORT_NUDGE_CONSECUTIVE: u32 = 8;
/// Bir işte bu sayıda ayrı tekrar serisi uyarı eşiğine ulaşırsa aynı yönlendirme eklenir.
pub(crate) const REPORT_NUDGE_DETECTIONS: u32 = 3;

const OUTPUT_MAX: usize = 300;
const INPUT_MAX: usize = 160;
/// Çağrının kimliğini değiştirmeyen üst-düzey alanlar (Bash'in görünen açıklaması gibi).
const DROPPED_KEYS: &[&str] = &["description", "message"];
/// Başarısı "bir şey değişti" demek olan araçlar: başka çağrıların iş toplamını sıfırlar,
/// böylece düzeltme sonrası aynı testi koşmak takılma sayılmaz.
const MUTATING: &[&str] = &["Edit", "Write", "NotebookEdit", "MultiEdit"];

struct State {
    delegation_id: i64,
    last_key: Option<String>,
    consecutive: u32,
    totals: HashMap<String, u32>,
    detections: u32,
    last_output: Option<String>,
    last_output_key: Option<String>,
    noted_total_keys: HashSet<String>,
}

/// Lidere not gerektiren eşik aşımı; girdi özeti kısaltılmış ve sırları ayıklanmıştır.
pub(crate) struct Stuck {
    pub tool: String,
    pub input_summary: String,
    pub consecutive: u32,
    pub total: u32,
}

pub(crate) struct Notice {
    pub context: String,
    pub stuck: Option<Stuck>,
}

/// Agent → current job's repeat state. A new job starts it empty.
#[derive(Default)]
pub struct Guard(Mutex<HashMap<String, State>>);

fn state_for<'a>(all: &'a mut HashMap<String, State>, agent: &str, delegation_id: i64) -> &'a mut State {
    let s = all.entry(agent.to_string()).or_insert_with(|| State {
        delegation_id,
        last_key: None,
        consecutive: 0,
        totals: HashMap::new(),
        detections: 0,
        last_output: None,
        last_output_key: None,
        noted_total_keys: HashSet::new(),
    });
    if s.delegation_id != delegation_id {
        *s = State {
            delegation_id,
            last_key: None,
            consecutive: 0,
            totals: HashMap::new(),
            detections: 0,
            last_output: None,
            last_output_key: None,
            noted_total_keys: HashSet::new(),
        };
    }
    s
}

/// Bir bekleme aracının birebir tekrarı tarifin kendisi; alt-ajan çağrıları ana akışın
/// serisini kirletmez. İkisi de sayılmaz.
fn wanted(body: &Value) -> Option<String> {
    if body["agent_id"].is_string() {
        return None;
    }
    let tool = body["tool_name"].as_str()?;
    if tool.ends_with("wait_answer") {
        return None;
    }
    Some(tool.to_string())
}

fn canonical(v: &Value, top: bool) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map
                .keys()
                .filter(|k| !(top && DROPPED_KEYS.contains(&k.as_str())))
                .collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", Value::String((*k).clone()), canonical(&map[*k], false)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(|i| canonical(i, false)).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn signature(tool: &str, body: &Value) -> String {
    let input = body.get("tool_input").unwrap_or(&Value::Null);
    format!("{tool}\0{}", canonical(input, true))
}

fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// Kısa, sırları ayıklanmış girdi özeti. Lider notuna girdi ham haliyle gitmez.
fn input_summary(body: &Value) -> String {
    let input = body.get("tool_input").unwrap_or(&Value::Null);
    cut(&crate::permit::redact(&canonical(input, true)), INPUT_MAX)
}

/// Aracın son gerçek çıktısının kısa özeti (pentagi dersi: uyarı asıl sonucu gösterir).
fn output_summary(body: &Value, failed: bool) -> String {
    let raw = if failed && !body["error"].is_null() {
        &body["error"]
    } else {
        &body["tool_response"]
    };
    let text = match raw {
        Value::String(s) => s.clone(),
        Value::Object(map) => {
            let picked: Vec<&str> = ["stdout", "stderr", "content", "text", "output", "error", "result"]
                .iter()
                .filter_map(|k| map.get(*k).and_then(Value::as_str))
                .filter(|s| !s.trim().is_empty())
                .collect();
            if picked.is_empty() { raw.to_string() } else { picked.join(" ") }
        }
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let collapsed = crate::permit::redact(&text).split_whitespace().collect::<Vec<_>>().join(" ");
    cut(&collapsed, OUTPUT_MAX)
}

impl Guard {
    /// PreToolUse: sayar. Farklı bir çağrı seriyi sıfırlar (araya giren Edit/Write da öyle).
    pub(crate) fn pre(&self, agent: &str, delegation_id: i64, body: &Value) {
        let Some(tool) = wanted(body) else { return };
        let key = signature(&tool, body);
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let s = state_for(&mut all, agent, delegation_id);
        if s.last_key.as_deref() == Some(key.as_str()) {
            s.consecutive += 1;
        } else {
            s.last_key = Some(key.clone());
            s.consecutive = 1;
        }
        *s.totals.entry(key).or_default() += 1;
    }

    /// PostToolUse/PostToolUseFailure: son çıktıyı tutar; seri eşikteyse uyarı metnini döner.
    pub(crate) fn post(&self, agent: &str, delegation_id: i64, body: &Value, failed: bool) -> Option<Notice> {
        let tool = wanted(body)?;
        let key = signature(&tool, body);
        let summary = output_summary(body, failed);
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let s = state_for(&mut all, agent, delegation_id);
        s.last_output = Some(summary.clone());
        s.last_output_key = Some(key.clone());
        if !failed && MUTATING.contains(&tool.as_str()) {
            s.totals.retain(|k, _| k == &key);
            s.noted_total_keys.retain(|k| k == &key);
        }
        if s.last_key.as_deref() != Some(key.as_str()) {
            return None;
        }
        let (consecutive, total) = (s.consecutive, s.totals.get(&key).copied().unwrap_or(0));
        // Toplam eşiği seri kısa olsa da geçerli (araya başka çağrılar giren yavaş döngü).
        let total_hit = total >= STUCK_TOTAL && s.noted_total_keys.insert(key.clone());
        if consecutive < REPEAT_WARN && !total_hit {
            return None;
        }
        if consecutive == REPEAT_WARN {
            s.detections += 1;
        }
        let summary_in = input_summary(body);
        let stuck = if consecutive == STUCK_CONSECUTIVE || total_hit {
            Some(Stuck { tool: tool.clone(), input_summary: summary_in.clone(), consecutive, total })
        } else {
            None
        };
        let kept = if s.last_output_key.as_deref() == Some(key.as_str()) {
            s.last_output.clone().unwrap_or_default()
        } else {
            summary
        };
        let output = if kept.is_empty() { "(çıktı boş)".to_string() } else { kept };
        let how = if consecutive >= REPEAT_WARN {
            format!("art arda {consecutive}. kez")
        } else {
            format!("bu işte toplam {total}. kez")
        };
        let mut context = format!(
            "[Dex tekrar bekçisi] Aynı çağrıyı {how} yapıyorsun: {tool} {summary_in}\n\
             Son gerçek çıktı (özet): {output}\n\
             Aynı çağrı aynı sonucu verecek; girdiyi ya da aracı değiştir."
        );
        if consecutive >= REPORT_NUDGE_CONSECUTIVE || s.detections >= REPORT_NUDGE_DETECTIONS {
            context.push_str(
                "\nBu işte tekrar üst sınıra yaklaştı: ilerleyemiyorsan kısmi sonucu `report` ile teslim et; aynı çağrıyı sürdürme.",
            );
        }
        Some(Notice { context, stuck })
    }

    /// Yeni tur (UserPromptSubmit): seri sıfırlanır, iş toplamları kalır.
    pub(crate) fn reset_turn(&self, agent: &str) {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = all.get_mut(agent) {
            s.last_key = None;
            s.consecutive = 0;
        }
    }

    /// Oturum/ajan bitti: durumu bırak.
    pub(crate) fn forget(&self, agent: &str) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(agent);
    }
}

impl crate::Core {
    fn loop_guard_job(&self, agent: &str) -> Option<i64> {
        crate::db::with(&self.db, |c| crate::delegate::running_id(c, agent)).ok().flatten()
    }

    pub(crate) fn loop_guard_pre(&self, agent: &str, body: &Value) {
        let Some(job) = self.loop_guard_job(agent) else { return };
        self.loop_guard.pre(agent, job, body);
    }

    /// Uyarı metni varsa döner; takılma eşiği aşıldıysa işin liderine not bırakır.
    pub(crate) fn loop_guard_post(&self, agent: &str, body: &Value, failed: bool) -> Option<String> {
        let job = self.loop_guard_job(agent)?;
        let notice = self.loop_guard.post(agent, job, body, failed)?;
        if let Some(stuck) = &notice.stuck {
            let text = format!(
                "Worker takılmış olabilir: {agent} (iş #{job}) {} çağrısını aynı girdiyle art arda {} kez, toplam {} kez tekrarladı. Girdi özeti: {}",
                stuck.tool, stuck.consecutive, stuck.total, stuck.input_summary
            );
            let leader = crate::db::with(&self.db, |c| {
                let d = crate::delegate::get(c, job)?;
                crate::plan::note(c, &d.from_agent, &crate::dexmsg::line(crate::dexmsg::SISTEM, &text))?;
                Ok(d.from_agent)
            });
            match leader {
                Ok(leader) => self.leader_notes_wake(&leader),
                Err(err) => eprintln!("dex loop guard leader note: {err}"),
            }
        }
        Some(notice.context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(tool: &str, input: Value, response: Value) -> Value {
        json!({ "tool_name": tool, "tool_input": input, "tool_response": response })
    }

    fn call(g: &Guard, agent: &str, job: i64, b: &Value) -> Option<Notice> {
        g.pre(agent, job, b);
        g.post(agent, job, b, false)
    }

    #[test]
    fn third_identical_call_warns_with_last_output() {
        let g = Guard::default();
        let b = body("Bash", json!({ "command": "cargo test x" }), json!({ "stdout": "1 failed" }));
        assert!(call(&g, "kaan", 7, &b).is_none());
        assert!(call(&g, "kaan", 7, &b).is_none());
        let n = call(&g, "kaan", 7, &b).expect("third repeat warns");
        assert!(n.context.contains("art arda 3. kez"), "{}", n.context);
        assert!(n.context.contains("1 failed"), "last real output rides along: {}", n.context);
        assert!(n.context.contains("girdiyi ya da aracı değiştir"));
        assert!(n.stuck.is_none());
    }

    #[test]
    fn key_order_and_description_do_not_make_a_new_call() {
        let g = Guard::default();
        let a = body("Bash", json!({ "command": "ls", "description": "listeler", "timeout": 5 }), json!("ok"));
        let b = body("Bash", json!({ "timeout": 5, "description": "başka yazı", "command": "ls" }), json!("ok"));
        assert!(call(&g, "kaan", 7, &a).is_none());
        assert!(call(&g, "kaan", 7, &b).is_none());
        assert!(call(&g, "kaan", 7, &a).is_some(), "normalized inputs count as the same call");
    }

    #[test]
    fn different_input_never_warns() {
        let g = Guard::default();
        for i in 0..6 {
            let b = body("Read", json!({ "file_path": format!("f{i}.rs") }), json!("ok"));
            assert!(call(&g, "kaan", 7, &b).is_none());
        }
    }

    #[test]
    fn edit_in_between_resets_the_streak_and_the_job_total() {
        let g = Guard::default();
        let test = body("Bash", json!({ "command": "cargo test x" }), json!("fail"));
        let edit = body("Edit", json!({ "file_path": "a.rs", "old_string": "a", "new_string": "b" }), json!("ok"));
        // 9 test koşusu, aralarda düzeltme: meşru TDD döngüsü, ne uyarı ne takılma.
        for _ in 0..3 {
            for _ in 0..3 {
                // Aynı grupta bile seri, Edit sonrasındaki ilk koşudan başlar; grup içi 3. koşu
                // uyarır (araya bir şey girmedi) ama toplam Edit'le sıfırlandığı için takılma olmaz.
                let n = call(&g, "kaan", 7, &test);
                assert!(n.as_ref().is_none_or(|n| n.stuck.is_none()));
            }
            assert!(call(&g, "kaan", 7, &edit).is_none());
        }
        // Edit'ten sonra seri yeniden sayar: ilk iki koşu sessiz.
        assert!(call(&g, "kaan", 7, &test).is_none());
        assert!(call(&g, "kaan", 7, &test).is_none());
    }

    #[test]
    fn stuck_note_at_five_consecutive_and_at_ten_total() {
        let g = Guard::default();
        let b = body("Bash", json!({ "command": "x" }), json!("err"));
        let mut stuck_at = Vec::new();
        for i in 1..=10 {
            if let Some(n) = call(&g, "kaan", 7, &b) {
                if n.stuck.is_some() {
                    stuck_at.push(i);
                }
                if i >= REPORT_NUDGE_CONSECUTIVE {
                    assert!(n.context.contains("`report`"), "report nudge at {i}: {}", n.context);
                }
            }
        }
        assert_eq!(stuck_at, vec![5, 10], "one note at the consecutive threshold, one at the total");

        // Toplam eşiği, araya başka çağrı girse de (Edit değil) iş içinde birikir.
        let g = Guard::default();
        let other = body("Read", json!({ "file_path": "x.rs" }), json!("ok"));
        let mut noted = false;
        for _ in 0..5 {
            for _ in 0..2 {
                if let Some(n) = call(&g, "kaan", 7, &b) {
                    noted |= n.stuck.is_some();
                }
            }
            assert!(call(&g, "kaan", 7, &other).is_none());
        }
        assert!(noted, "the tenth identical call of the job notes the leader");
    }

    #[test]
    fn turn_reset_and_forget_clear_state() {
        let g = Guard::default();
        let b = body("Bash", json!({ "command": "x" }), json!("err"));
        assert!(call(&g, "kaan", 7, &b).is_none());
        assert!(call(&g, "kaan", 7, &b).is_none());
        g.reset_turn("kaan");
        assert!(call(&g, "kaan", 7, &b).is_none(), "a new turn restarts the streak");
        assert!(call(&g, "kaan", 7, &b).is_none());
        g.forget("kaan");
        assert!(call(&g, "kaan", 7, &b).is_none(), "forget drops everything");
        // Yeni iş de eski işin serisini taşımaz.
        assert!(call(&g, "kaan", 7, &b).is_none());
        assert!(call(&g, "kaan", 8, &b).is_none(), "job change resets");
        assert!(call(&g, "kaan", 8, &b).is_none());
        assert!(call(&g, "kaan", 8, &b).is_some());
    }

    #[test]
    fn summaries_are_redacted_and_cut() {
        let g = Guard::default();
        let long = format!("token=abc {}", "ş".repeat(400));
        let b = body("Bash", json!({ "command": "x token=abc" }), json!({ "stdout": long }));
        for _ in 0..2 {
            call(&g, "kaan", 7, &b);
        }
        let n = call(&g, "kaan", 7, &b).unwrap();
        assert!(n.context.contains("[GİZLİ]"), "{}", n.context);
        assert!(!n.context.contains("token=abc"));
        assert!(n.context.chars().count() < 700, "{}", n.context.chars().count());
    }

    /// Hook yolu: 3. tekrarda PostToolUse cevabı bağlam taşır, 5. tekrarda lidere not düşer;
    /// çalışan iş yoksa hiçbir şey sayılmaz.
    #[test]
    fn core_hook_path_warns_and_notes_the_leader() {
        use crate::{db::with, delegate, events, Core, Events, Paths};
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp")
            .join(format!("loop-guard-{}-{}", std::process::id(), crate::agents::next_seq()));
        let c = Core::start(Paths { data_dir: root.join("data"), config_dir: root.join("config") }, Events::new(events::NullSink)).unwrap();
        let call = json!({ "tool_name": "Bash", "tool_input": { "command": "cargo test x" }, "tool_response": { "stdout": "1 failed" } });
        for _ in 0..3 {
            c.delegation_on_hook("kaan", "PreToolUse", &call, false);
            c.delegation_on_hook("kaan", "PostToolUse", &call, false);
        }
        let job = with(&c.db, |db| {
            let from = delegate::caller(db, "timur")?;
            let d = delegate::insert(db, delegate::NewDelegation { from: &from, to_agent: "kaan".into(), team_id: "default".into(), service_team_id: None, prompt: "iş".into(), task_id: None, patron_words: None, effort: None })?;
            delegate::set_status(db, d.id, "running")?;
            Ok(d.id)
        }).unwrap();
        let mut warned = None;
        for i in 1..=5 {
            c.delegation_on_hook("kaan", "PreToolUse", &call, false);
            let out = c.delegation_on_hook("kaan", "PostToolUse", &call, false);
            if i < 3 {
                assert!(out.as_deref().is_none_or(|o| !o.contains("tekrar bekçisi")), "call {i}: {out:?} (no job counted before)");
            } else if i == 3 {
                warned = out;
            }
        }
        let warned = warned.expect("third repeat carries context");
        let v: Value = serde_json::from_str(&warned).unwrap();
        let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert!(ctx.contains("1 failed") && ctx.contains("girdiyi ya da aracı değiştir"), "{ctx}");
        let notes: Vec<String> = with(&c.db, |db| {
            let mut st = db.prepare("SELECT text FROM leader_notes WHERE agent_id = 'timur'").map_err(|e| e.to_string())?;
            let rows = st.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
        }).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains(&format!("iş #{job}")) && notes[0].contains("kaan") && notes[0].contains("Bash"), "{}", notes[0]);
        c.shutdown();
    }
}
