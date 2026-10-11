import { test } from "node:test";
import assert from "node:assert/strict";
import { renderMarkdown } from "./md.ts";
import { buildFeed, currentTool, reconcileOutgoing, type Outgoing } from "./group.ts";
import { parsePrompt } from "./prompt.ts";
import type { ChatMessage } from "./api.ts";

const msg = (id: string, seq: number, role: string, text = "", extra: Partial<ChatMessage> = {}): ChatMessage => ({
  id, seq, role, text, ...extra,
});
const tool = (id: string, seq: number, name: string, ok?: boolean | null) =>
  msg(id, seq, "tool", "", { tool: { name, input: "x", ok } });

test("markdown: escapes html and keeps headings, bold, italic, code", () => {
  const h = renderMarkdown("# Başlık\n\n<script>x</script> **kalın** *eğik* `a<b`");
  assert.match(h, /<h1>Başlık<\/h1>/);
  assert.ok(!h.includes("<script>"));
  assert.match(h, /<strong>kalın<\/strong>/);
  assert.match(h, /<em>eğik<\/em>/);
  assert.match(h, /<code>a&lt;b<\/code>/);
});

test("markdown: links — safe ones only, bare urls linked", () => {
  const h = renderMarkdown("[x](https://a.io/p?q=1&r=2) [bad](javascript:alert(1)) bkz https://b.io/yol.");
  assert.match(h, /<a href="https:\/\/a\.io\/p\?q=1&amp;r=2"[^>]*>x<\/a>/);
  assert.ok(!h.includes('href="javascript'));
  assert.match(h, /<a href="https:\/\/b\.io\/yol"/);
});

test("markdown: fenced code keeps content literal and carries the language", () => {
  const h = renderMarkdown("```ts\nconst a = 1 < 2;\n**x**\n```\nson");
  assert.match(h, /<pre data-lang="ts"><button[^>]*data-copy>Kopyala<\/button><code>const a = 1 &lt; 2;\n\*\*x\*\*<\/code><\/pre>/);
  assert.match(h, /<p>son<\/p>/);
});

test("markdown: unclosed fence does not swallow forever / crash", () => {
  assert.match(renderMarkdown("```\nkod"), /<pre><button[^>]*data-copy>Kopyala<\/button><code>kod<\/code><\/pre>/);
});

test("markdown: table", () => {
  const h = renderMarkdown("| a | b |\n|---|--:|\n| 1 | 2 |\n| 3 | 4 |");
  assert.match(h, /<th style="text-align:left">a<\/th><th style="text-align:right">b<\/th>/);
  assert.equal((h.match(/<tr>/g) ?? []).length, 3);
});

const md = (...lines: string[]) => renderMarkdown(lines.join("\n"));

test("markdown: table cells keep escaped pipes and pipes inside code; short rows are padded", () => {
  const h = md("| komut | not |", "|---|---|", "| `a | b` | x \\| y |", "| tek |");
  assert.match(h, /<td style="text-align:left"><code>a \| b<\/code><\/td><td style="text-align:left">x \| y<\/td>/);
  assert.match(h, /<td style="text-align:left">tek<\/td><td style="text-align:left"><\/td><\/tr>/);
});

test("markdown: ASCII table becomes a real table with a header", () => {
  const h = md(
    "Sonuç:",
    "+--------+--------+------+",
    "| paket  | durum  | süre |",
    "+--------+--------+------+",
    "| lint   | OLDU   | 3 sn |",
    "| e2e    | HATA   |41 sn |",
    "+--------+--------+------+",
    "son",
  );
  assert.match(h, /^<p>Sonuç:<\/p><div class="chat-table"><table><thead><tr><th[^>]*>paket<\/th><th[^>]*>durum<\/th><th[^>]*>süre<\/th><\/tr><\/thead>/);
  assert.equal((h.match(/<tr>/g) ?? []).length, 3);
  assert.match(h, /<td[^>]*>e2e<\/td><td[^>]*>HATA<\/td><td[^>]*>41 sn<\/td>/);
  assert.match(h, /<\/table><\/div><p>son<\/p>$/);
});

test("markdown: box-drawn table (│ ┼) and a frame without a header rule", () => {
  const box = md("┌─────┬─────┐", "│ a   │ b   │", "├─────┼─────┤", "│ 1   │ 2   │", "└─────┴─────┘");
  assert.match(box, /<thead><tr><th[^>]*>a<\/th><th[^>]*>b<\/th><\/tr><\/thead><tbody><tr><td[^>]*>1<\/td><td[^>]*>2<\/td><\/tr><\/tbody>/);
  const plain = md("+---+---+", "| 1 | 2 |", "| 3 | 4 |", "+---+---+");
  assert.ok(!plain.includes("<thead>"));
  assert.equal((plain.match(/<tr>/g) ?? []).length, 2);
});

test("markdown: ASCII-looking text stays text — inside code, a lone rule, rows without rules", () => {
  assert.ok(!md("```", "+---+", "| a |", "+---+", "```").includes("<table>"));
  assert.ok(!md("+---+", "metin").includes("<table>"));
  assert.ok(!md("| a |", "| b |").includes("<table>"));
});

test("markdown: nested and mixed lists", () => {
  const h = renderMarkdown("- bir\n  - iç\n  - iç2\n- iki\n\n1. x\n2. y");
  assert.equal(h, "<ul><li>bir<ul><li>iç</li><li>iç2</li></ul></li><li>iki</li></ul><ol><li>x</li><li>y</li></ol>");
});

test("markdown: blockquote, hr, task box", () => {
  const h = renderMarkdown("> alıntı\n\n---\n- [x] bitti\n- [ ] kaldı");
  assert.match(h, /<blockquote><p>alıntı<\/p><\/blockquote><hr>/);
  assert.match(h, /☑ bitti/);
  assert.match(h, /☐ kaldı/);
});

test("markdown: 10,000 quote markers stay bounded and overflow is escaped plain text", () => {
  const h = renderMarkdown(">".repeat(10_000) + ' <script>"&"</script> **literal**\n> son');
  const depth = (h.match(/<blockquote>/g) ?? []).length;
  assert.ok(depth > 0 && depth <= 32);
  assert.ok(!h.includes("<script>") && !h.includes("<strong>"));
  assert.match(h, /&lt;script&gt;&quot;&amp;&quot;&lt;\/script&gt; \*\*literal\*\*/);
  assert.ok(h.includes("&gt;"));
  assert.match(h, /son/);
});

test("feed: consecutive tools/thinking collapse into one group; messages break it", () => {
  const feed = buildFeed([
    msg("u", 1, "user", "selam"),
    tool("t1", 2, "Bash", true),
    msg("th", 3, "assistant", "hmm", { thinking: true }),
    tool("t2", 4, "Read", false),
    tool("t3", 5, "Edit", null),
    msg("a", 6, "assistant", "bitti"),
    tool("t4", 7, "Bash", true),
  ]);
  assert.deepEqual(feed.map((f) => f.kind), ["msg", "activity", "msg", "activity"]);
  const g = feed[1] as Extract<(typeof feed)[number], { kind: "activity" }>;
  assert.equal(g.tools.length, 3);
  assert.equal(g.thoughts.length, 1);
  assert.equal(g.failed, 1);
  assert.equal(g.running, true);
});

test("feed: day separators appear when the day changes", () => {
  const d1 = new Date(2026, 0, 1, 10).getTime();
  const d2 = new Date(2026, 0, 2, 10).getTime();
  const feed = buildFeed([msg("a", 1, "user", "x", { ts: d1 }), msg("b", 2, "assistant", "y", { ts: d1 + 1000 }), msg("c", 3, "user", "z", { ts: d2 })]);
  assert.deepEqual(feed.map((f) => f.kind), ["day", "msg", "msg", "day", "msg"]);
});

test("currentTool: latest call since the last user message", () => {
  assert.equal(currentTool([msg("u", 1, "user", "x"), tool("t", 2, "Grep", null)])?.name, "Grep");
  assert.equal(currentTool([tool("t", 1, "Grep", true), msg("u", 2, "user", "x")]), null);
});

test("outgoing: dropped once the transcript echoes it (whitespace-insensitive); older rows don't count", () => {
  const o: Outgoing = { id: "o1", text: "merhaba  dünya", afterSeq: 5, at: 1000, state: "sent" };
  const old = msg("x", 3, "user", "merhaba dünya");
  assert.equal(reconcileOutgoing([o], [old], 2000).length, 1);
  assert.equal(reconcileOutgoing([o], [old, msg("y", 6, "user", "merhaba dünya\n")], 2000).length, 0);
});

test("outgoing: sent bubbles expire, sending/failed stay", () => {
  const base = { text: "a", afterSeq: 0, at: 0 };
  const list: Outgoing[] = [
    { id: "1", state: "sent", ...base },
    { id: "2", state: "sending", ...base },
    { id: "3", state: "failed", ...base },
  ];
  assert.deepEqual(reconcileOutgoing(list, [], 120_000).map((x) => x.id), ["2", "3"]);
});

test("prompt: claude permission menu with highlighted option → digit keys, no Enter", () => {
  const screen = [
    "╭──────────────────────────╮",
    "│ Bash command             │",
    "│   rm -rf build           │",
    "│ Do you want to proceed?  │",
    "│ ❯ 1. Yes                 │",
    "│   2. Yes, and don't ask again │",
    "│   3. No, and tell Claude what to do differently (esc) │",
    "╰──────────────────────────╯",
  ].join("\n");
  const p = parsePrompt(screen)!;
  assert.equal(p.kind, "choice");
  assert.match(p.question, /Do you want to proceed\?/);
  assert.deepEqual(p.options.map((o) => o.keys), ["1", "2", "3"]);
  assert.equal(p.options[0].label, "Yes");
});

test("prompt: unmarked numbered list is a text prompt → digit + Enter; older menus ignored", () => {
  const p = parsePrompt("1. eski\n2. eski2\nbaşka çıktı\nSeç:\n1. a\n2. b\n3. c")!;
  assert.deepEqual(p.options.map((o) => o.label), ["a", "b", "c"]);
  assert.deepEqual(p.options.map((o) => o.keys), ["1\r", "2\r", "3\r"]);
});

test("prompt: y/n question and plain text fallback", () => {
  const yn = parsePrompt("Devam edilsin mi? (y/n)")!;
  assert.equal(yn.kind, "yesno");
  assert.deepEqual(yn.options.map((o) => o.keys), ["y\r", "n\r"]);
  const txt = parsePrompt("Enter your API key:")!;
  assert.equal(txt.kind, "text");
  assert.equal(txt.options.length, 0);
  assert.equal(parsePrompt("  \n "), null);
});
