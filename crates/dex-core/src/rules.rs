//! DEX-35: what an agent's CLI gets as its system prompt — Katman 1 (the role text from
//! `seed/roles.json`) + Katman 2 (`DEX_RULES`, one block for every agent). Claude takes it as
//! `--append-system-prompt`, built by `system_prompt`.
//! Event-time texts (DENY_ASK, worker_prompt, results_text, …) are hook answers, not this.

use rusqlite::{Connection, OptionalExtension};
use std::path::Path;

/// Katman 2 (source: `docs/roles-v2/_dex-kurallari.md`). `- [Lider] ` / `- [Worker] ` lines go
/// only to that kind, tag stripped; `- [QA] ` only to a QA worker; `- [İnceleyici] ` only to a code-review worker (#286), `- [-İnceleyici] ` to everyone else (#636); `{shared}` / `{own}` are the memory dirs, `{roles}` the role catalog (leader only, built at spawn). `## Hafıza` is
/// dropped when the agent has no memory dir.
pub const DEX_RULES: &str = "# Dex çalışma kuralları
Patron: insan, son söz onun. Lider işi böler ve devreder; worker devredilen işi yapar.

## Çalışma
- Yazmadan önce ilgili kodu oku; çevredeki koda benze, mevcut olanı yeniden kullan.
- En küçük doğru değişikliği yap; istenmeyen refactor ve kapsam dışı iş yok.
- Sırrı koda/log'a yazma, kişisel veriyi loglama.
- [Worker] Çıktı sözleşmesi: rapor içeriğini `report` ile ver, dosyayı kayıtlı proje kökünde Dex yazar; geçici iş çalışma dizininin `.dex/tmp/` klasöründe veya oturum scratchpad'inde, derleme çıktısı verilmişse `CARGO_TARGET_DIR` içinde, diğer derleme/APK/kurulum paketleri çalışma dizininin `.dex/release/<desktop|android>/` klasöründe kalır; bu izinli yollar dışında dosya/klasör açma, geçici worktree gerekiyorsa `.dex/tmp/` altına koy.
- Geri alınamaz ya da riskli adım (silme, canlıya alma, toplu veri değişikliği, harcama) açık onay ister.
- [Worker] Onayı ve belirsizliği patrona değil liderine sor: `ask_leader` (AskUserQuestion reddedilir). Soru sor, cevabı aynı turda bekle: `ask_leader` bloklar; zaman aşımında `wait_answer` (questionId) ile beklemeyi sürdür, turu bitirme. Cevaba göre işe devam et; `report`'u cevap geldikten ve iş bittikten sonra ver; cevaplanmamış sorun varsa reddedilir.
- [Worker] İş metninde patronun sözü liderin açıklamasından üstündür; çelişki ya da arada kalma varsa varsayım yapma, `ask_leader` ile sor.
- [Lider] Onayı patrondan al. Worker sorusunu `answer` ile cevapla; karar patronunsa `escalate`. Soruyu cevapsız bırakma.
- [Lider] Durum mesajlarında grupları şu başlıklarla yaz: `Senden beklenen:` (patron kararı/eylemi), `Süren:`, `Biten:` — her biri satır başında, altında madde listesi.

## Araçlar (dex MCP)
- Pano: `task_list`, `task_create`, `task_update`. Devredilen işin görevini sen güncelle.
- [Worker] Pano kartı açma; sana verilen işin kartı Dex tarafından açılır, yalnız onu `task_update` ile güncelle.
- [Worker] Teslim: `report` (delegationId, content=<tam rapor>, tek satır summary); dosyayı Dex yazar, rapor için dosya yazma. content ile resultPath birlikte verilmez; eski dosya teslimi resultPath ile korunur. Sohbet mesajın lidere gitmez. Cevap beklemiyorsan raporsuz durma: iş \"raporsuz bitti\" sayılır.
- [Lider] Tek bağımsız iş `delegate`; birden çok ya da sıralı iş `plan` (`dependsOn`). Durum `plan_status`, iptal `plan_cancel`, düşen görev `plan_retry`. Gereken rol yoksa `propose_member` (patron onaylar). İki worker aynı dosyaya dokunmasın.
- [Lider] İşi varsayılan olarak `role` ile ver; Dex o roldeki boş worker'a dağıtır. `agentId` yalnız düzeltme turunda (`fixOf`) ya da bağlamı o worker'da olan işte.
- [Lider] İş patronun bir isteğinden doğuyorsa patronun ilgili sözünü `patronWords`'e birebir koy. Bitti tanımına \"patron ne görecek\" satırı ekle.
- [Lider] Çalışan işe düzeltme/ek bilgi için `note` (delegationId, text); yeni iş için `delegate` kullan.
- [Lider] delegate/plan'da her iş için `effort` seç (medium|high|xhigh); GLM'de medium = high, xhigh = GLM max; Haiku'da etkisiz.
- [Lider] delegate/plan'da `model` belirtmek patron onayı ister; `modelReason` ver, yalnız gerektiğinde kullan.
- [Lider] Worker işi kendi git worktree dalında yaparsa sonuçta `dal:` satırı gelir. Takımda code-review üyesi varsa Dex dalı ona otomatik inceletir ve `delegation_merge` yalnız dal ucunun kararı `onay` ise birleştirir (inceleme sürüyorsa ya da `düzelt` ise birleştirmeden döner); üye yoksa inceleme yok, doğrudan birleştirir. Çakışma ya da politika dosyası değişikliği birleştirmeden döner. `forceReview` yalnız patron onayıyla.
- [Lider] İnceleme `düzelt` derse düzeltme işini aynı worker'a `delegate` ile `fixOf: <düzeltilen devretme>` vererek ver; yeni dal eskisinin ucunda açılır ve yeniden incelenir.
- [Lider] Rol kataloğu: {roles}. `propose_member`/`delegate` role yalnız bunlardan; uygun rol yoksa yakın rolle önermeden önce patrona sor (`escalate`).
- Sonuç, soru ve cevaplar bağlamına kendiliğinden gelir; yoklama yapma.

## Test bütçesi
- Komutlar \"Proje yığını\" bölümünden; yoksa projenin kendi betiklerinden.
- Varsayılan: tip kontrolü + lint + ilgili birim testleri.
- Tam test paketi — hangi araçla olursa olsun (tarayıcı/uçtan uca, oyun motoru, mobil emülatör, …) — yalnız QA'nın işi (plan sonunda ya da patron isteyince); diğer roller yalnız ilgili tek dosya/test.
- Uygulayıcı örnekleri: Playwright'ta tarayıcı/uçtan uca test yalnız UI akışını değiştiren işte ve yalnız ilgili tek test; Godot GUT'ta ilgili tek dosya/test. Kayıtlı uygulayıcılar tam paketi QA dışına reddeder.
- [QA] QA: bilinmeyen/plansız bir tam-paket çalıştırıcı görürsen `ask_leader` ile \"kural planı öner\" yükseltmesi yap.
- [Lider] Lider tarayıcı/e2e testi koşmaz (politika reddeder); kendi küçük işinde yalnız tip kontrolü.
- [Lider] Test bütçesi planı eksikse patrona öner. Patron onayıyla zamanı geldiğinde plan yazdır. Otomatik kural ekleme yok.
- Kırmızıyı testi gevşeterek yeşile çevirme.

## Teslim
- [Worker] Rapor/kanıt/ekran görüntüsünü repoya commit'leme; bu çıktılar `.dex/` altında kalır.
- [Worker] Ayrı kod incelemesi/inceleyici isteme; skill aksini söylese de commit'li işinin incelemesini Dex merge öncesi otomatik açar. Testlerini koş, commit'le, report ver.
- [İnceleyici] İnceleme işinde kod yazma, commit atma; yalnız oku ve rapor içeriğini `report` aracının content alanıyla teslim et. Raporun ilk satırı `karar: onay` ya da `karar: düzelt`; bulgular `## önemli` ve `## küçük` altında `dosya:satır — kısa gerekçe`. Önemli bulgu varsa karar düzelt.
- [İnceleyici] Kabuk: yalnız literal argümanlı basit komutlar, `|` `;` `&&` `||` ile bağlanır (`cd`, `head`, `2>&1`, `2>/dev/null` olur). Değişken, `$( )`, ters tırnak, atama, döngü, `( )`/`{ }` grubu, `&` ve dosyaya yazan `>` reddedilir — sha'yı önce ayrı komutla al, sonra literal kullan. Main ile çakışmayı `delegation_merge` yakalar; `git merge-tree` deneme.
- Ekran görüntüsü/kanıt dosyası üretme (istenmedikçe).
- Teslim = ne yaptın + koştuğun komutlar ve sonucu (geçti/kaldı, sayı) + açık risk/soru. Kısa.
- Koşmadığın komutu koştum deme; koşamadıysan nedenini yaz.

## Hafıza
Takım ortak: {shared} · Sana özel: {own}. İndeks oturum başında gelir; ayrıntı için dosyayı oku.
- İşe başlarken ilgili konuda `memory_search` ile ara.
- [-İnceleyici] Bir bilgi = bir .md; dosya adı = `name`. Frontmatter: name, description (tek satır), metadata.type (project|feedback|reference|user). [[slug]] ile bağla. MEMORY.md tutma.
- [-İnceleyici] Yalnız kalıcı ve ileride işe yarayanı yaz: kök neden, karar+gerekçe, tuzak, tercih. Kodda/git'te olanı yazma.
- [-İnceleyici] Aynı konuda kayıt varsa güncelle; yanlışı sil. Takımı ilgilendiren ortak klasöre.
- [-İnceleyici] İş bitince sor: \"Kalıcı ne öğrendim?\" Varsa yaz, yoksa geç.
- [İnceleyici] Hafızaya yazma (salt okumasın, reddedilir); kalıcı bilgi varsa raporunun sonunda `## not` altına yaz, lider taşır.";

/// Placed after this `## Hafıza` line (absent for the reviewer) only when the context has its own scopes.
const SCOPE_ANCHOR: &str = "- Aynı konuda kayıt varsa güncelle; yanlışı sil. Takımı ilgilendiren ortak klasöre.";
const SCOPE_TAIL: &str = "kadro/genel çalışma tercihi → takım shared; ajan alışkanlığı → private. Aynı slug farklı scope'larda olabilir; scope/proje id'siyle nitelikli bağlantı kullan, kör [[slug]] birleşimi yapma.";

/// `DEX_RULES` for one agent. `kind` = `agents.kind`; unknown kind gets neither tagged line.
/// `memory` = (shared, own) dirs; `roles` = `roles::catalog_text` (only the leader's line carries it).
/// `role` = `agents.role_slug`.
pub fn dex_rules(kind: Option<&str>, role: Option<&str>, memory: Option<(&Path, &Path)>, roles: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_memory = false;
    for line in DEX_RULES.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            in_memory = h == "Hafıza";
        }
        if in_memory && memory.is_none() {
            continue;
        }
        let reviewer = kind == Some("worker") && role == Some(crate::review::ROLE);
        if let Some(rest) = line.strip_prefix("- [QA] ") {
            if kind == Some("worker") && role == Some("qa") {
                out.push(format!("- {rest}"));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("- [İnceleyici] ") {
            if reviewer {
                out.push(format!("- {rest}"));
            }
            continue;
        }
        let line = match line.strip_prefix("- [-İnceleyici] ") {
            Some(_) if reviewer => continue,
            Some(rest) => format!("- {rest}"),
            None => line.to_string(),
        };
        let line = match (line.strip_prefix("- [Lider] "), line.strip_prefix("- [Worker] ")) {
            (Some(rest), _) if kind == Some("leader") => format!("- {rest}"),
            (Some(_), _) => continue,
            (_, Some(rest)) if kind == Some("worker") => format!("- {rest}"),
            (_, Some(_)) => continue,
            _ => line,
        };
        out.push(line);
    }
    let text = out.join("\n").trim_end().replace("{roles}", roles);
    match memory {
        Some((shared, own)) => text
            .replace("{shared}", &shared.display().to_string())
            .replace("{own}", &own.display().to_string()),
        None => text,
    }
}

/// Katman 1 + Katman 2 for the agent; the whole system-prompt text Dex adds. Order: role text,
/// `## Proje yığını` (resolved repo root `.dex/stack.md`, #322), rules, leader extras (GPU, stack note).
pub fn system_prompt(conn: &Connection, agent: &str, memory: Option<(&Path, &Path)>) -> String {
    build_system_prompt(conn, agent, memory, crate::stack::team_cwd(conn, agent).as_deref(), None)
}

/// G passes the already resolved, authoritative launch/session context (never UI defaults).
pub fn system_prompt_for_context(conn: &Connection, agent: &str, ctx: &crate::project_context::ProjectContext) -> String {
    if ctx.agent_id.as_deref() != Some(agent) {
        return String::new();
    }
    let shared = ctx.team_memory_root.join("shared");
    let own = crate::slug::memory_dir(conn, &ctx.agent_memory_root, agent).ok();
    build_system_prompt(conn, agent, own.as_deref().map(|o| (shared.as_path(), o)), ctx.project_root.as_deref(), Some(ctx))
}

fn build_system_prompt(
    conn: &Connection,
    agent: &str,
    memory: Option<(&Path, &Path)>,
    cwd: Option<&Path>,
    ctx: Option<&crate::project_context::ProjectContext>,
) -> String {
    let (kind, role, test_level, team_profile): (Option<String>, Option<String>, String, String) = conn
        .query_row("SELECT a.kind, a.role_slug, t.test_level, t.profile FROM agents a JOIN teams t ON t.id=a.team_id WHERE a.id = ?1", [agent], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()
        .ok()
        .flatten()
        .unwrap_or((None, None, "full".into(), "dev".into()));
    let education = team_profile == "edu";
    let assistant = role.as_deref() == Some("assistant");
    if assistant {
        let mut rules = assistant_rules(memory);
        if let Some(ctx) = ctx { rules.push_str(&context_block(ctx)); }
        return format!("{}\n\n{rules}", crate::roles::prompt_for_agent(conn, agent).unwrap_or_default());
    }
    let roles = if kind.as_deref() == Some("leader") { crate::roles::catalog_text_for_profile(conn, &team_profile) } else { String::new() };
    let mut rules = dex_rules(kind.as_deref(), role.as_deref(), memory, &roles);
    if education { rules = education_rules(&rules); }
    if !education && test_level == "light" {
        if let (Some(start), Some(end)) = (rules.find("## Test bütçesi"), rules.find("## Teslim")) {
            let leader_note = if kind.as_deref() == Some("leader") { "\n- Hafif test düzeyinde QA tam turu planlama." } else { "" };
            rules.replace_range(start..end, &format!("## Test bütçesi\n- Test düzeyi: hafif — yalnız derleme + tip kontrolü (ör. cargo check, tsc --noEmit); birim, tarayıcı/uçtan uca ve QA tam paket yok; patron uygulamada test eder.{leader_note}\n\n"));
        }
    }
    // The old/single-project text (project share = team share, or no project path given) stays
    // as it was; scope routing appears only where a context really splits the shares.
    if let Some(ctx) = ctx {
        let service = ctx.project_team_id.as_deref().is_some_and(|t| t != ctx.team_id);
        let project_shared = ctx.project_memory_root.as_ref().map(|r| r.join("shared"));
        let separate = project_shared.as_ref().filter(|p| !service && !crate::memory::same_dir(p, &ctx.team_memory_root.join("shared")));
        let head = if service {
            Some("isteyen projenin hafızasına yazma; repo bulgularını rapora koy;")
        } else if let Some(project) = separate {
            rules = rules.replace("Takım ortak:", &format!("Proje ortak: {} · Takım ortak:", project.display()));
            Some("repo kök nedeni/teknoloji → proje shared;")
        } else if project_shared.is_none() && bound_projects(conn, &ctx.team_id) > 1 {
            Some("bu bağlamda proje seçili değil; repo bilgisini takım shared'e otomatik yazma;")
        } else {
            None
        };
        if let Some(head) = head {
            rules = rules.replacen(SCOPE_ANCHOR, &format!("{SCOPE_ANCHOR}
- Scope: {head} {SCOPE_TAIL}"), 1);
        }
    }
    // Görev #289: only the leader hears of the GPU resource (no address/token), only when connected.
    if kind.as_deref() == Some("leader") {
        if let Some(gpu) = crate::gpu::leader_rules(conn) {
            rules = format!(
                "{rules}

{gpu}"
            );
        }
    }
    // #322: the repo's stack profile sits between the role and the rules; the leader alone hears
    // that it is missing (with what the repo looks like) or was cut.
    let profile = cwd.filter(|_| !education).and_then(crate::stack::read);
    if !education && kind.as_deref() == Some("leader") {
        if let Some(note) = cwd.and_then(|c| crate::stack::leader_note(c, profile.as_ref())) {
            rules = format!("{rules}\n\n{note}");
        }
        if let Some(root) = cwd.filter(|_| test_level != "light") {
            let unplanned = crate::test_budget::unplanned_runners(root);
            if !unplanned.is_empty() {
                let project = ctx.and_then(|c| c.project_name.as_deref()).map(String::from)
                    .unwrap_or_else(|| root.display().to_string()).replace(['\r', '\n'], " ");
                rules.push_str(&format!("\n\nTest bütçesi planı yok: {} ({project}) — patrona kural planı önermeyi düşün.", unplanned.join(", ")));
            }
        }
    }
    if let Some(p) = &profile {
        rules = format!("{}\n\n{rules}", crate::stack::section(p));
    }
    // Plan #99 mapgen: harita dosyası varsa tek satır (edu/asistan hariç); yoksa istem baytı değişmez.
    if !education && !assistant {
        if let Some(root) = cwd {
            if let Some(line) = crate::repomap::prompt_line(root) {
                rules = format!("{rules}\n\n{line}");
            }
        }
    }
    if let Some(ctx) = ctx {
        rules.push_str(&context_block(ctx));
    }
    match crate::roles::prompt_for_agent(conn, agent) {
        Some(role) => format!("{role}\n\n{rules}"),
        None => rules,
    }
}

/// Reuse the tagged mechanics and memory text verbatim; the dev source/rendering stays unchanged.
/// Prefixes refer to whole, controlled rule lines, never role text or user-provided content.
fn education_rules(rules: &str) -> String {
    let mut in_test_budget = false;
    let mut out = Vec::new();
    for line in rules.lines() {
        if line.starts_with("## ") { in_test_budget = line == "## Test bütçesi"; }
        if in_test_budget { continue; }
        if [
            "- Yazmadan önce", "- En küçük doğru", "- Durum mesajlarında",
            "- Worker işi kendi git", "- İnceleme `düzelt`", "- Rapor/kanıt/ekran",
            "- Ayrı kod incelemesi", "- Koşmadığın komutu",
        ].iter().any(|prefix| line.starts_with(prefix)) { continue; }
        out.push(if line.starts_with("- Çıktı sözleşmesi:") {
            "- Çıktı sözleşmesi: ders dosyaları takım klasöründe `lessons/` altında kalır; rapor içeriğini `report` ile ver, dosyayı kayıtlı proje kökünde Dex yazar; geçici iş takım klasörünün `.dex/tmp/` klasöründe veya oturum scratchpad'inde kalır."
        } else if line.starts_with("- İşi varsayılan olarak `role`") {
            "- İşi varsayılan olarak `role` ile ver; Dex o roldeki boş worker'a dağıtır. `agentId` yalnız bağlamı o worker'da olan işte."
        } else if line.starts_with("- Teslim =") {
            "- Teslim = ders dosyasının yolu + ne gösterdiği + açık risk/soru. Kısa."
        } else { line });
    }
    out.join("\n")
}

fn assistant_rules(memory: Option<(&Path, &Path)>) -> String {
    let mut out = vec!["# Dex çalışma kuralları".to_string(), "Patron: insan, son söz onun.".into(), "".into(), "## Çalışma".into()];
    for line in DEX_RULES.lines().filter(|l| l.starts_with("- Sırrı") || l.starts_with("- Geri alınamaz")) { out.push(line.into()); }
    out.push("- Cevabını kendi panelinde doğrudan patrona ver; onayı ve belirsizliği patrona sor.".into());
    out.push("- Salt okursun: dosya yazma, kabuk/kod/betik çalıştırma, git ve kurulum yok. Web üzerinden kaynakları oku.".into());
    if let Some((shared, own)) = memory {
        out.push(format!("\n## Hafıza\nTakım ortak: {} · Sana özel: {}. İndeks oturum başında gelir; ayrıntı için dosyayı oku.", shared.display(), own.display()));
        out.push("- İşe başlarken ilgili konuda `memory_search` ile ara.".into());
        out.push("- Hafıza salt okunur; kalıcı tercihi patrona söyle.".into());
    }
    out.join("\n")
}

fn bound_projects(conn: &Connection, team: &str) -> usize {
    crate::projects::list(conn, team, false).map_or(0, |l| l.iter().filter(|p| p.root_path.is_some()).count())
}

fn context_block(ctx: &crate::project_context::ProjectContext) -> String {
    let mut job = format!("\n\n## İş/proje bağlamı\nTakım: {}\n", ctx.team_id);
    if let Some(p) = &ctx.project_id {
        job.push_str(&format!("Proje: {p} ({})\n", ctx.project_name.as_deref().unwrap_or("")));
    }
    if let Some(t) = &ctx.project_team_id {
        job.push_str(&format!("İsteyen takım: {t}\n"));
    }
    if let Some(id) = ctx.delegation_id {
        job.push_str(&format!("İş: {id}\n"));
    }
    if let Some(root) = &ctx.project_root {
        job.push_str(&format!("Repo kökü: {}\n", root.display()));
    }
    if let Some(branch) = &ctx.merge_branch {
        job.push_str(&format!("Hedef dal: {branch}\n"));
    }
    if let Some(report) = &ctx.result_path {
        job.push_str(&format!("Rapor: {}\n", report.display()));
    }
    if ctx.project_team_id.as_deref().is_some_and(|t| t != ctx.team_id) {
        job.push_str("İsteyen proje stack'i salt okunur; hafızasına yazamazsın.\n");
    }
    job
}

/// Whole rows only; header and overflow count are included in the 2 KiB budget.
pub fn project_list_block(conn: &Connection, team: &str, limit: usize) -> Result<String, String> {
    let limit = limit.min(2048);
    let projects = crate::projects::list(conn, team, true).map_err(|e| e.to_string())?;
    let mut out = String::from("# Takım projeleri\n");
    let note = |n| format!("… {n} proje sığmadı; project_list ile sayfalı devam et.\n");
    if out.len() + note(projects.len()).len() > limit {
        return Ok(String::new());
    }
    let mut dropped = 0;
    for p in &projects {
        let state = if p.archived_at.is_some() {
            "arşiv"
        } else if p.root_path.is_none() {
            "bağlı değil"
        } else {
            "aktif"
        };
        let row = format!("- {} — {} — {state} — {}\n", p.id, p.name.replace(['\r', '\n'], " "), p.default_branch.as_deref().unwrap_or("—"));
        if out.len() + row.len() + note(projects.len()).len() <= limit {
            out.push_str(&row);
        } else {
            dropped += 1;
        }
    }
    if dropped > 0 {
        out.push_str(&note(dropped));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_prompt_is_direct_and_read_only_without_developer_process() {
        let c = db();
        c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).unwrap();
        let (s, o) = mem();
        let root = project("assistant", &[(crate::stack::FILE, "DEV STACK")]);
        let t = build_system_prompt(&c, "ayla", Some((&s, &o)), Some(&root), None);
        assert!(t.starts_with("Rolün: Patronun kişisel asistanı."), "{t}");
        for excluded in ["report", "ask_leader", "patronWords", "Test bütçesi", "Proje yığını", "DEV STACK", "worktree", "commit", "Hafızaya yaz", "Bir bilgi ="] {
            assert!(!t.contains(excluded), "{excluded}: {t}");
        }
        for kept in ["Patron: insan", "Sırrı", "riskli adım", "## Hafıza", "memory_search", "kendi panelinde", "belirsizliği patrona"] { assert!(t.contains(kept), "{kept}: {t}"); }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn education_prompts_keep_roles_memory_and_mechanics_without_dev_rules() {
        let c = db();
        c.execute("UPDATE teams SET profile='edu' WHERE id='default'", []).unwrap();
        c.execute("UPDATE agents SET role_slug='teacher' WHERE id='timur'", []).unwrap();
        c.execute("UPDATE agents SET role_slug='visual-explainer' WHERE id='ayla'", []).unwrap();
        let (s, o) = mem();
        let root = project("edu", &[(crate::stack::FILE, "DEV STACK"), ("package.json", "{\"scripts\":{\"e2e\":\"playwright test\"}}")]);
        for (agent, role) in [("timur", "Öğretmen"), ("ayla", "Görsel Anlatıcı")] {
            let t = build_system_prompt(&c, agent, Some((&s, &o)), Some(&root), None);
            assert!(t.starts_with(&format!("Rolün: {role}")) && t.contains("## Hafıza"), "{t}");
            for excluded in ["Test bütçesi", "delegation_merge", "Proje yığını", "DEV STACK", "worktree", "fixOf", "kod incelemesi", "Senden beklenen:", "Yazmadan önce", "En küçük doğru", "koştuğun komutlar"] {
                assert!(!t.contains(excluded), "edu contains {excluded}: {t}");
            }
            assert!(t.contains("Sırrı koda/log'a yazma") && t.contains("riskli adım") && t.contains("`memory_search`"));
            let dev_memory = dex_rules(Some(if agent == "timur" { "leader" } else { "worker" }), None, Some((&s, &o)), "");
            assert_eq!(t.split("## Hafıza").nth(1), dev_memory.split("## Hafıza").nth(1));
            if agent == "timur" {
                assert!(t.contains("teacher — Teacher, visual-explainer — Visual Explainer") && !t.contains("backend —"));
                for tool in ["`delegate`", "`note`", "`answer`", "`escalate`", "`effort`", "`model`", "`patronWords`"] { assert!(t.contains(tool), "{tool}"); }
            } else {
                assert!(t.contains("`lessons/`") && t.contains("content=<tam rapor>") && t.contains("`ask_leader`") && t.contains("`wait_answer`"));
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dev_prompt_bytes_match_pre_education_baseline() {
        use sha2::{Digest, Sha256};
        let c = db();
        let (s, o) = mem();
        for (agent, expected) in [("timur", "5e39c1ff89c7ffafffc6adc85ebd072c369ab16bfe62ce01ee22f247c5581a75"), ("ayla", "45bc508726debaff9f101322d3c9f9997bcf588d5306c808f315fe522bcab209")] {
            let t = system_prompt(&c, agent, Some((&s, &o)));
            assert_eq!(format!("{:x}", Sha256::digest(t.as_bytes())), expected, "{agent} prompt bytes changed");
        }
    }

    #[test]
    fn education_prompt_bytes_match_pre_assistant_baseline() {
        use sha2::{Digest, Sha256};
        let c=db();
        c.execute("UPDATE teams SET profile='edu' WHERE id='default'",[]).unwrap();
        c.execute("UPDATE agents SET role_slug='teacher' WHERE id='timur'",[]).unwrap();
        c.execute("UPDATE agents SET role_slug='visual-explainer' WHERE id='ayla'",[]).unwrap();
        let (s,o)=mem();
        // Hashes rendered from HEAD's DEX_RULES and education_rules before #1697.
        for (agent,expected) in [("timur","9eba00ea42973623fea350f314078e53cba7a0d59e99af034006966cc90498db"),("ayla","7315f32118b4d0279b07c8d05204240dfb39f224a20a27553352535ed6132f75")] {
            assert_eq!(format!("{:x}",Sha256::digest(system_prompt(&c,agent,Some((&s,&o))).as_bytes())),expected,"{agent}");
        }
    }

    #[test]
    fn full_test_budget_is_general_and_qa_escalates_missing_plans() {
        for role in ["backend", "qa", "code-review"] {
            let text = dex_rules(Some("worker"), Some(role), None, "");
            assert!(text.contains("Tam test paketi — hangi araçla olursa olsun"), "{text}");
            assert!(text.contains("diğer roller yalnız ilgili tek dosya/test"), "{text}");
            assert_eq!(text.contains("bilinmeyen/plansız bir tam-paket çalıştırıcı"), role == "qa", "{text}");
        }
        let qa = dex_rules(Some("worker"), Some("qa"), None, "");
        assert!(qa.contains("`ask_leader` ile \"kural planı öner\""));
        let leader = dex_rules(Some("leader"), Some("qa"), None, "");
        assert!(!leader.contains("bilinmeyen/plansız bir tam-paket çalıştırıcı"));
        assert!(leader.contains("Patron onayıyla zamanı geldiğinde plan yazdır"));
    }

    #[test]
    fn report_content_instructions_include_host_owned_outputs() {
        for role in ["backend", "code-review"] {
            let text = dex_rules(Some("worker"), Some(role), None, "");
            assert!(text.contains("content"), "{text}");
            assert!(text.contains("Dex yazar"), "{text}");
            assert!(text.contains("CARGO_TARGET_DIR"), "{text}");
            assert!(!text.contains("Raporun tamamını önce devretme metnindeki dosyaya"));
        }
    }

    #[test]
    fn unplanned_runner_note_is_leader_only_and_refreshes_at_prompt_build() {
        let c = db();
        let f = crate::projects::tests::Fixture::new();
        let root = f.dir("runner");
        std::fs::create_dir_all(root.join(".dex")).unwrap();
        std::fs::write(root.join(crate::stack::FILE), "STACK EXISTS").unwrap();
        std::fs::write(root.join("cypress.config.ts"), "").unwrap();
        set_cwd(&c, &root);
        let note = format!("Test bütçesi planı yok: Cypress ({}) — patrona kural planı önermeyi düşün.", root.display());
        assert!(system_prompt(&c, "timur", None).contains(&note));
        for agent in ["ayla", "bora"] {
            assert!(!system_prompt(&c, agent, None).contains("Test bütçesi planı yok:"));
        }
        let mut ctx = crate::project_context::resolve(
            &c, &f.0, crate::project_context::ResolveRequest {
                team_id: "default", agent_id: Some("timur"), delegation_id: None,
                project_id: None, mode: crate::project_context::ContextMode::Team,
            },
        ).unwrap();
        ctx.project_name = Some("Runner Project".into());
        let other = f.dir("other");
        set_cwd(&c, &other);
        assert!(system_prompt_for_context(&c, "timur", &ctx)
            .contains("Test bütçesi planı yok: Cypress (Runner Project) — patrona kural planı önermeyi düşün."));
        assert!(!system_prompt(&c, "timur", None).contains("Test bütçesi planı yok:"));
        set_cwd(&c, &root);
        std::fs::remove_file(root.join("cypress.config.ts")).unwrap();
        std::fs::write(root.join("playwright.config.ts"), "").unwrap();
        std::fs::write(root.join("project.godot"), "").unwrap();
        std::fs::create_dir_all(root.join("addons/gut")).unwrap();
        assert!(!system_prompt(&c, "timur", None).contains("Test bütçesi planı yok:"));
    }

    /// v1 addenda (DEX-22/24/27, DEX-3 memory protocol) must not come back.
    const OLD: &[&str] = &[
        "# Dex lider araçları",
        "# Dex: soru kuralı",
        "# Dex: worker soruları",
        "# Dex hafıza protokolü",
        "her zaman `--workers=2`",
        "Keşif'te",
    ];

    fn db() -> Connection {
        crate::db::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn explicit_context_uses_repo_stack_and_scoped_memory() {
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(&f.0.join("db")).unwrap();
        let root = f.dir("repo");
        std::fs::create_dir_all(root.join(".dex")).unwrap();
        std::fs::write(root.join(crate::stack::FILE), "REPO STACK").unwrap();
        let prepared = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
        let p = crate::projects::create(&c, "default", crate::projects::NewProject { name: "One".into(), worktrees: None }, &prepared).unwrap();
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,project_id,project_root,created_at,updated_at) VALUES(1,'timur','ayla','default','x',?1,?2,1,1)", rusqlite::params![p.id, p.root_path]).unwrap();
        let ctx = crate::project_context::resolve(
            &c,
            &f.0,
            crate::project_context::ResolveRequest {
                team_id: "default",
                agent_id: Some("ayla"),
                delegation_id: Some(1),
                project_id: None,
                mode: crate::project_context::ContextMode::Selection,
            },
        )
        .unwrap();
        let text = system_prompt_for_context(&c, "ayla", &ctx);
        assert!(text.find("Rolün:").unwrap() < text.find("REPO STACK").unwrap());
        assert!(text.find("REPO STACK").unwrap() < text.find("# Dex çalışma kuralları").unwrap());
        assert!(text.contains(&format!("Proje: {}", p.id)) && text.contains("İş: 1"));
        assert!(text.contains("Proje ortak:") && text.contains("kadro/genel çalışma tercihi → takım shared"));
        c.execute("UPDATE agents SET role_slug='code-review' WHERE id='ayla'", []).unwrap();
        let reviewer = system_prompt_for_context(&c, "ayla", &ctx);
        assert!(reviewer.contains("Hafızaya yazma") && !reviewer.contains("repo kök nedeni/teknoloji →"));
        let root2 = f.dir("other");
        let prepared2 = crate::projects::prepare_root(root2.to_str().unwrap(), None).unwrap();
        crate::projects::create(&c, "default", crate::projects::NewProject { name: "Two".into(), worktrees: None }, &prepared2).unwrap();
        assert!(!system_prompt(&c, "timur", None).contains("REPO STACK"));
        let list = project_list_block(&c, "default", 2048).unwrap();
        assert!(list.contains(&p.id) && list.len() <= 2048);
        assert!(project_list_block(&c, "default", 100).unwrap().len() <= 100);
        for i in 0..20 {
            c.execute(
                "INSERT INTO projects(id,team_id,name,created_at,updated_at) VALUES(?1,'default',?2,1,1)",
                rusqlite::params![format!("list-{i}"), "ü".repeat(500)],
            )
            .unwrap();
        }
        let listing = project_list_block(&c, "default", 2048).unwrap();
        assert!(listing.len() <= 2048 && listing.contains("project_list ile sayfalı"));
        let dropped: usize = listing.rsplit("… ").next().unwrap().split_whitespace().next().unwrap().parse().unwrap();
        assert_eq!(listing.lines().filter(|l| l.starts_with("- ")).count() + dropped, crate::projects::list(&c, "default", true).unwrap().len());

        let service = crate::teams::create_team(
            &c,
            &f.0,
            crate::teams::NewTeam { profile: None, name: "Service".into(), kind: "service".into(), cwd: None, worker_roles: None, members: None },
        )
        .unwrap();
        let worker = crate::teams::list_agents(&c).unwrap().into_iter().find(|a| a.team_id == service.id && a.kind == "worker").unwrap();
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,service_team_id,prompt,project_id,project_root,created_at,updated_at) VALUES(2,'timur',?1,'default',?2,'x',?3,?4,1,1)", rusqlite::params![worker.id, service.id, p.id, p.root_path]).unwrap();
        let service_ctx = crate::project_context::resolve(
            &c,
            &f.0,
            crate::project_context::ResolveRequest {
                team_id: &service.id,
                agent_id: Some(&worker.id),
                delegation_id: Some(2),
                project_id: None,
                mode: crate::project_context::ContextMode::Selection,
            },
        )
        .unwrap();
        let service_prompt = system_prompt_for_context(&c, &worker.id, &service_ctx);
        assert!(service_prompt.contains("REPO STACK") && service_prompt.contains("İsteyen proje stack'i salt okunur"));
        assert!(service_prompt.contains(&service_ctx.team_memory_root.join("shared").display().to_string()));
        assert!(!service_prompt.contains(&service_ctx.project_memory_root.unwrap().join("shared").display().to_string()));
    }

    #[test]
    fn scope_line_only_where_project_share_is_given() {
        assert!(!DEX_RULES.contains("Scope:") && DEX_RULES.contains(&SCOPE_ANCHOR.replacen("- ", "- [-İnceleyici] ", 1)));
        let f = crate::projects::tests::Fixture::new();
        let c = crate::db::open(&f.0.join("db")).unwrap();
        let (s, o) = mem();
        let legacy = system_prompt(&c, "ayla", Some((&s, &o)));
        assert!(legacy.contains(SCOPE_ANCHOR) && !legacy.contains("Scope:"), "legacy spawn text unchanged");
        let team_ctx = || {
            crate::project_context::resolve(
                &c,
                &f.0,
                crate::project_context::ResolveRequest {
                    team_id: "default",
                    agent_id: Some("ayla"),
                    delegation_id: None,
                    project_id: None,
                    mode: crate::project_context::ContextMode::Team,
                },
            )
            .unwrap()
        };
        let scope = |ctx: &crate::project_context::ProjectContext| {
            let t = system_prompt_for_context(&c, "ayla", ctx);
            assert!(t.contains(SCOPE_ANCHOR));
            t.lines().find(|l| l.starts_with("- Scope:")).map(String::from)
        };
        let mut ctx = team_ctx();
        ctx.project_memory_root = Some(ctx.team_memory_root.clone());
        assert_eq!(scope(&ctx), None, "single project: project share = team share");
        assert!(!system_prompt_for_context(&c, "ayla", &ctx).contains("Proje ortak:"));
        ctx.project_memory_root = None;
        assert_eq!(scope(&ctx), None, "no project path, one bound project at most");
        ctx.project_memory_root = Some(f.0.join("repo/.dex/memory"));
        let split = system_prompt_for_context(&c, "ayla", &ctx);
        assert!(split.contains(&format!("{SCOPE_ANCHOR}\n- Scope: repo kök nedeni/teknoloji → proje shared; {SCOPE_TAIL}")), "{split}");
        assert!(split.contains("Proje ortak:"));
        for name in ["one", "two"] {
            let root = f.dir(name);
            let prepared = crate::projects::prepare_root(root.to_str().unwrap(), None).unwrap();
            crate::projects::create(&c, "default", crate::projects::NewProject { name: name.into(), worktrees: None }, &prepared).unwrap();
        }
        let unselected = team_ctx();
        assert!(unselected.project_memory_root.is_none());
        assert!(scope(&unselected).unwrap().contains("proje seçili değil"));
        assert!(!system_prompt(&c, "ayla", Some((&s, &o))).contains("Scope:"), "legacy text stays without a project path");
    }

    fn mem() -> (std::path::PathBuf, std::path::PathBuf) {
        (Path::new("/m/shared").into(), Path::new("/m/agents/x").into())
    }

    fn assert_common(t: &str) {
        assert!(t.contains("# Dex çalışma kuralları"), "{t}");
        assert!(t.contains("## Test bütçesi") && t.contains("yalnız ilgili tek test"), "{t}");
        // Commands remain in the stack profile; tool names are examples below the general rule.
        assert!(DEX_RULES.contains("Uygulayıcı örnekleri: Playwright") && DEX_RULES.contains("Godot GUT"));
        for w in ["playwright test", "--workers", "--grep", "video/trace", "npx", "tsc"] {
            assert!(!DEX_RULES.contains(w), "ortak kurallarda {w}\n{t}");
        }
        assert!(t.contains("Takım ortak: /m/shared · Sana özel: /m/agents/x"), "{t}");
        assert!(!t.contains("{shared}") && !t.contains("{own}") && !t.contains("{roles}") && !t.contains("[Lider]") && !t.contains("[Worker]"), "{t}");
        for o in OLD {
            assert!(!t.contains(o), "eski ek: {o}\n{t}");
        }
    }

    #[test]
    fn backend_worker_gets_role_and_worker_rules() {
        let c = db();
        let (s, o) = mem();
        let t = system_prompt(&c, "ayla", Some((&s, &o)));
        assert!(t.starts_with("Rolün: Backend Mühendisi."), "{t}");
        assert_common(&t);
        assert!(t.contains("- Onayı ve belirsizliği patrona değil liderine sor: `ask_leader`"), "{t}");
        assert!(t.contains("- Teslim: `report`"), "{t}");
        assert!(t.contains("Ayrı kod incelemesi/inceleyici isteme; skill aksini söylese de commit'li işinin incelemesini Dex merge öncesi otomatik açar. Testlerini koş, commit'le, report ver."), "{t}");
        assert!(!t.contains("`karar: onay`"), "inceleyici satırı backend'e gitti
{t}");
        assert!(t.contains("patronun sözü liderin açıklamasından üstündür"), "{t}");
        assert!(!t.contains("`plan_retry`") && !t.contains("`escalate`") && !t.contains("`patronWords`") && !t.contains("`effort` seç"), "lider satırı worker'a gitti\n{t}");
        assert!(!t.contains("Durum mesajlarında grupları"), "lider durum grupları worker'a gitti\n{t}");
    }

    #[test]
    fn lead_gets_role_and_leader_rules() {
        let c = db();
        let (s, o) = mem();
        let t = system_prompt(&c, "timur", Some((&s, &o)));
        assert!(t.starts_with("Rolün: Takım Lideri."), "{t}");
        assert_common(&t);
        assert!(t.contains("- Tek bağımsız iş `delegate`") && t.contains("`plan_retry`"), "{t}");
        assert!(t.contains("- Onayı patrondan al."), "{t}");
        assert!(t.contains("Durum mesajlarında grupları şu başlıklarla yaz: `Senden beklenen:` (patron kararı/eylemi), `Süren:`, `Biten:` — her biri satır başında, altında madde listesi."), "{t}");
        assert!(t.contains("`patronWords`'e birebir koy") && t.contains("\"patron ne görecek\""), "{t}");
        assert!(t.contains("her iş için `effort` seç (medium|high|xhigh); GLM'de medium = high, xhigh = GLM max; Haiku'da etkisiz."), "{t}");
        assert!(t.contains("yalnız dal ucunun kararı `onay` ise birleştirir") && t.contains("`fixOf: <düzeltilen devretme>`"), "{t}");
        assert!(!t.contains("`karar: onay`"), "inceleyici satırı lidere gitti
{t}");
        assert!(!t.contains("liderin açıklamasından üstündür"), "worker satırı lidere gitti\n{t}");
        assert!(!t.contains("`ask_leader`") && !t.contains("- Teslim: `report`"), "worker satırı lidere gitti\n{t}");
        assert!(!t.contains("Ayrı kod incelemesi/inceleyici isteme"), "worker satırı lidere gitti\n{t}");
    }

    #[test]
    fn leader_gets_catalog_built_from_the_table_worker_does_not() {
        let c = db();
        let (s, o) = mem();
        let t = system_prompt(&c, "timur", Some((&s, &o)));
        let all = crate::roles::list(&c).unwrap();
        assert!(all.len() > 10);
        for r in all.iter().filter(|r| r.category != "education" && r.slug != "assistant") {
            assert!(t.contains(&format!("{} — {}", r.slug, r.name)), "{} yok\n{t}", r.slug);
        }
        assert!(t.contains("mobile — Mobile Engineer"), "{t}");
        assert!(t.contains("uygun rol yoksa yakın rolle önermeden önce patrona sor (`escalate`)"), "{t}");
        // Not a fixed list: a catalog change shows up in the next spawn.
        c.execute("INSERT INTO roles (slug, name, category, description, system_prompt, sort) VALUES ('mobile-x', 'Mobil Test', 'x', '', '', 999)", []).unwrap();
        assert!(system_prompt(&c, "timur", Some((&s, &o))).contains("mobile-x — Mobil Test"));
        let w = system_prompt(&c, "ayla", Some((&s, &o)));
        assert!(!w.contains("Rol kataloğu") && !w.contains("backend — Backend") && !w.contains("mobile-x"), "{w}");
        println!("katalog metni: {} karakter", crate::roles::catalog_text(&c).chars().count());
    }

    #[test]
    fn code_review_worker_gets_the_report_format() {
        let c = db();
        c.execute("UPDATE agents SET role_slug = 'code-review' WHERE id = 'ayla'", []).unwrap();
        let (s, o) = mem();
        let t = system_prompt(&c, "ayla", Some((&s, &o)));
        assert!(t.starts_with("Rolün: Kod İnceleyici."), "{t}");
        assert!(t.contains("- İnceleme işinde kod yazma, commit atma; yalnız oku ve rapor içeriğini `report` aracının content alanıyla teslim et. Raporun ilk satırı `karar: onay` ya da `karar: düzelt`"), "{t}");
        assert!(t.contains("`## önemli` ve `## küçük`") && !t.contains("[İnceleyici]"), "{t}");
        assert!(!t.contains("`fixOf"), "lider satırı inceleyiciye gitti
{t}");
        // #636: the reviewer reads memory but is not told to write it; it hands notes to the leader.
        assert!(t.contains("Takım ortak: /m/shared · Sana özel: /m/agents/x. İndeks oturum başında gelir"), "{t}");
        for w in MEMORY_WRITE {
            assert!(!t.contains(w), "yazma satırı inceleyiciye gitti: {w}
{t}");
        }
        assert!(t.ends_with("
- Hafızaya yazma (salt okumasın, reddedilir); kalıcı bilgi varsa raporunun sonunda `## not` altına yaz, lider taşır."), "{t}");
        assert!(t.contains("- Kabuk: yalnız literal argümanlı basit komutlar") && t.contains("sha'yı önce ayrı komutla al, sonra literal kullan") && t.contains("`git merge-tree` deneme."), "{t}");
        assert!(!t.contains("İnceleyici]"), "{t}");
    }

    /// #636: everyone but the reviewer keeps the memory-writing lines, word for word.
    const MEMORY_WRITE: &[&str] = &[
        "- Bir bilgi = bir .md; dosya adı = `name`.",
        "- Yalnız kalıcı ve ileride işe yarayanı yaz",
        "- Aynı konuda kayıt varsa güncelle; yanlışı sil.",
        "- İş bitince sor: \"Kalıcı ne öğrendim?\" Varsa yaz, yoksa geç.",
    ];

    #[test]
    fn worker_and_leader_keep_memory_writing_lines() {
        let c = db();
        let (s, o) = mem();
        for id in ["ayla", "timur"] {
            let t = system_prompt(&c, id, Some((&s, &o)));
            assert!(t.contains("- İşe başlarken ilgili konuda `memory_search` ile ara."), "{id}");
            assert!(t.ends_with("- İş bitince sor: \"Kalıcı ne öğrendim?\" Varsa yaz, yoksa geç."), "{t}");
            for w in MEMORY_WRITE {
                assert!(t.contains(w), "{id}: {w}
{t}");
            }
            assert!(!t.contains("Hafızaya yazma (salt") && !t.contains("- Kabuk:") && !t.contains("İnceleyici]"), "inceleyici satırı {id}'e gitti
{t}");
        }
    }

    fn project(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dex-rules-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (f, body) in files {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn set_cwd(c: &Connection, d: &Path) {
        c.execute("UPDATE teams SET cwd = ?1 WHERE id = (SELECT team_id FROM agents WHERE id = 'timur')", [d.to_string_lossy()]).unwrap();
        c.execute("UPDATE projects SET root_path=?1,root_key=?2 WHERE id=(SELECT default_project_id FROM teams WHERE id='default')", rusqlite::params![d.to_string_lossy(), crate::projects::path_key(d.to_str().unwrap())]).unwrap();
    }

    #[test]
    fn stack_profile_sits_between_role_and_rules_for_every_agent() {
        let c = db();
        let d = project("profile", &[(crate::stack::FILE, "Godot 4.3, GDScript.
Test: `godot --headless -s addons/gut/gut_cmdln.gd`
")]);
        set_cwd(&c, &d);
        for id in ["timur", "ayla"] {
            let t = system_prompt(&c, id, None);
            let (role, sec, rules) = (t.find("Rolün:").unwrap(), t.find("## Proje yığını
Godot 4.3").unwrap(), t.find("# Dex çalışma kuralları").unwrap());
            assert!(role < sec && sec < rules, "{t}");
            assert!(t.contains("gut_cmdln.gd`

# Dex çalışma kuralları"), "{t}");
            assert!(!t.contains("Proje yığını notu"), "{t}");
        }
    }

    #[test]
    fn oversize_profile_is_cut_and_only_the_leader_is_warned() {
        let c = db();
        let d = project("oversize", &[(crate::stack::FILE, &"x".repeat(crate::stack::LIMIT + 10))]);
        set_cwd(&c, &d);
        let lead = system_prompt(&c, "timur", None);
        assert!(lead.contains(&format!("{}

# Dex", "x".repeat(crate::stack::LIMIT))) && !lead.contains(&"x".repeat(crate::stack::LIMIT + 1)), "kesilmedi");
        assert!(lead.ends_with(&format!("`.dex/stack.md` {} bayt; ilk {} baytı eklendi, gerisi kesildi. Patronla birlikte kısalt.", crate::stack::LIMIT + 10, crate::stack::LIMIT)), "{lead}");
        let worker = system_prompt(&c, "ayla", None);
        assert!(worker.contains("## Proje yığını
xxx") && !worker.contains("kesildi"), "{worker}");
    }

    #[test]
    fn no_profile_leader_gets_detection_note_worker_nothing() {
        let c = db();
        let g = project("godot", &[("project.godot", "")]);
        set_cwd(&c, &g);
        let lead = system_prompt(&c, "timur", None);
        assert!(lead.contains("## Proje yığını notu
Bu projede `.dex/stack.md` yok; tespit edilen: Godot. Patronla birlikte taslağı yaz"), "{lead}");
        assert!(lead.contains("test komutu, derleme/çalıştırma, kod kuralları, rollerin bu yığındaki karşılığı") && lead.contains("`!.dex/stack.md`"), "{lead}");
        assert!(!lead.contains("## Proje yığını
"), "{lead}");
        let worker = system_prompt(&c, "ayla", None);
        assert!(!worker.contains("## Proje yığını"), "{worker}");
        let t = project("tauri", &[("Cargo.toml", ""), ("package.json", "{}"), ("src-tauri/tauri.conf.json", "{}")]);
        set_cwd(&c, &t);
        assert!(system_prompt(&c, "timur", None).contains("tespit edilen: Tauri, Node/web."));
        // Service team / no folder: no note.
        c.execute("UPDATE teams SET cwd = NULL", []).unwrap();
        c.execute("UPDATE projects SET root_path=NULL,root_key=NULL", []).unwrap();
        assert!(!system_prompt(&c, "timur", None).contains("## Proje yığını"));
    }

    #[test]
    fn map_line_appears_only_for_dev_with_map_file() {
        let c = db();
        let (s, o) = mem();
        let d = project("map", &[]);
        set_cwd(&c, &d);
        for id in ["timur", "ayla"] {
            assert!(!system_prompt(&c, id, Some((&s, &o))).contains("Proje haritası:"), "{id}");
        }
        std::fs::create_dir_all(d.join(".dex")).unwrap();
        std::fs::write(d.join(crate::repomap::MAP_FILE), "x").unwrap();
        let expected = format!("Proje haritası: `{}/.dex/map.md` — iş karışık gelirse ya da nereden başlayacağını bilmiyorsan bak; hemen bakmak zorunda değilsin, işin ortasında da açabilirsin.", d.display());
        for id in ["timur", "ayla"] {
            assert!(system_prompt(&c, id, Some((&s, &o))).contains(&expected), "{id}");
        }
        c.execute("UPDATE teams SET profile='edu' WHERE id='default'", []).unwrap();
        assert!(!system_prompt(&c, "timur", Some((&s, &o))).contains("Proje haritası:"));
        c.execute("UPDATE teams SET profile='dev' WHERE id='default'", []).unwrap();
        c.execute("UPDATE agents SET role_slug='assistant' WHERE id='ayla'", []).unwrap();
        assert!(!system_prompt(&c, "ayla", Some((&s, &o))).contains("Proje haritası:"));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn no_memory_dir_drops_memory_section() {
        let t = dex_rules(Some("worker"), Some("backend"), None, "");
        assert!(!t.contains("## Hafıza") && !t.contains("{shared}"), "{t}");
        assert!(t.ends_with("koştuğun komutlar ve sonucu (geçti/kaldı, sayı) + açık risk/soru. Kısa.\n- Koşmadığın komutu koştum deme; koşamadıysan nedenini yaz."), "{t}");
    }

    #[test]
    fn unknown_kind_gets_no_tagged_line() {
        let t = dex_rules(None, None, None, "");
        assert!(!t.contains("ask_leader") && !t.contains("plan_retry"), "{t}");
    }
}
