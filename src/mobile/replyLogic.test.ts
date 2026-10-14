import { test } from "node:test";
import assert from "node:assert/strict";
import { isWebUrl, mergeReplies, parseReplyLink, pushRegisterNeeded, replyTaskLabel, replyTime, replyView, toMs } from "./replyLogic.ts";

test("deep link: dex://reply/<id> → id; numeric ids become numbers", () => {
  assert.equal(parseReplyLink("dex://reply/42"), 42);
  assert.equal(parseReplyLink("dex://reply/r_7-x"), "r_7-x");
  for (const bad of [null, undefined, 42, "", "dex://reply/", "dex://reply/a/b", "dex://reply/../x", "https://reply/1", "dex://reply/1?x=2", "dex://reply/" + "a".repeat(65)]) {
    assert.equal(parseReplyLink(bad), null, String(bad));
  }
});

test("push_register: once per host per token, only where advertised", () => {
  const base = { host: "h1", token: "t1", methods: ["push_register"], sentToken: undefined };
  assert.equal(pushRegisterNeeded(base), true);
  assert.equal(pushRegisterNeeded({ ...base, sentToken: "t1" }), false, "already sent");
  assert.equal(pushRegisterNeeded({ ...base, sentToken: "t0" }), true, "token refreshed");
  assert.equal(pushRegisterNeeded({ ...base, token: null }), false, "no FCM token");
  assert.equal(pushRegisterNeeded({ ...base, token: "" }), false);
  assert.equal(pushRegisterNeeded({ ...base, methods: ["inbox_submit"] }), false, "host lacks it");
  assert.equal(pushRegisterNeeded({ ...base, methods: null }), true, "unknown list: try, the error is swallowed");
  assert.equal(pushRegisterNeeded({ ...base, host: "local" }), false);
});

test("reply view: raw HTML is escaped, never rendered", () => {
  const { lead, body } = replyView("Sonuç: <b>tamam</b>\n\n<script>alert(1)</script>\n<img src=x onerror=alert(1)>");
  assert.equal(lead, "Sonuç: &lt;b&gt;tamam&lt;/b&gt;");
  assert.ok(!/<script|<img/i.test(body), body);
  assert.ok(body.includes("&lt;script&gt;"));
});

test("reply view: only http/https links survive", () => {
  const { body } = replyView("x\n\n[a](javascript:alert(1)) [b](mailto:a@b.c) [c](data:text/html,x) [d](https://ok.example/p) http://plain.example");
  const hrefs = [...body.matchAll(/href="([^"]*)"/g)].map((m) => m[1]);
  assert.deepEqual(hrefs, ["https://ok.example/p", "http://plain.example"]);
  assert.ok(!/javascript:|mailto:|data:text/.test(hrefs.join(" ")));
  assert.ok(body.includes(">b<") || body.includes("b"), "mailto text kept as text");
  const lead = replyView("[x](mailto:a@b.c) bitti").lead!;
  assert.ok(!lead.includes("<a"), lead);
});

test("reply view: first line is the lead card; headings/quotes unwrapped; code/list starts have none", () => {
  assert.deepEqual(replyView("\n# Repo: **dex** iyi\n\n- a\n- b").lead, "Repo: <strong>dex</strong> iyi");
  assert.equal(replyView("> Doğrulandı\nrest").lead, "Doğrulandı");
  const v = replyView("Tek cümle.\n\n## Ayrıntı\n\n```ts\nconst a = 1;\n```\n\n> alıntı");
  assert.equal(v.lead, "Tek cümle.");
  assert.ok(v.body.includes("<h2>Ayrıntı</h2>") && v.body.includes("<pre") && v.body.includes("<blockquote>"));
  assert.ok(!v.body.includes("Tek cümle"));
  for (const s of ["```\ncode\n```", "- madde", "1. madde", "| a | b |\n|---|---|"]) assert.equal(replyView(s).lead, null, s);
  assert.equal(replyView("").lead, null);
});

test("task labels", () => {
  assert.equal(replyTaskLabel("repo"), "Repo incele");
  assert.equal(replyTaskLabel("verify"), "Kaynak doğrula");
  assert.equal(replyTaskLabel("ask"), "Soru");
  assert.equal(replyTaskLabel(null), null);
  assert.equal(replyTaskLabel("toString"), null);
});

test("web url check", () => {
  assert.ok(isWebUrl("https://a.b") && isWebUrl("HTTP://a.b"));
  assert.ok(!isWebUrl("javascript:x") && !isWebUrl("mailto:a") && !isWebUrl(null));
});

test("time: seconds or ms; today / yesterday / older", () => {
  const now = new Date(2026, 9, 14, 21, 0).getTime();
  assert.equal(toMs(1_700_000_000), 1_700_000_000_000);
  assert.equal(toMs(1_700_000_000_000), 1_700_000_000_000);
  assert.equal(replyTime(new Date(2026, 9, 14, 9, 5).getTime(), now), "09:05");
  assert.equal(replyTime(new Date(2026, 9, 13, 23, 59).getTime() / 1000, now), "Dün 23:59");
  assert.equal(replyTime(new Date(2026, 9, 2, 7, 0).getTime(), now), "2 Eki 07:00");
});

test("merge: newest first, dedup per host+id, limit", () => {
  const r = (host: string, id: number, createdAt: number) => ({ host, id, createdAt, task: null, preview: "", agentId: "e", agentName: "Echo" });
  const out = mergeReplies([[r("a", 1, 10), r("a", 2, 30)], [r("b", 1, 20), r("a", 2, 30)]]);
  assert.deepEqual(out.map((x) => `${x.host}${x.id}`), ["a2", "b1", "a1"]);
  assert.equal(mergeReplies([[r("a", 1, 1), r("a", 2, 2), r("a", 3, 3)]], 2).length, 2);
});
