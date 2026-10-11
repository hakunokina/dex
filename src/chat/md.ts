// Chat Markdown → safe HTML. Pure (no DOM): everything is escaped first, only http(s)/mailto links.
// Supports headings, fenced code (lang), blockquote, hr, tables (pipe and ASCII/box-drawn), nested lists, task boxes,
// **bold**, *italic*, ~~strike~~, `code`, [text](url) and bare URLs.

const esc = (s: string) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

const SAFE_URL = /^(https?:\/\/|mailto:)/i;

function emphasis(s: string): string {
  return s
    .replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[^*\w])\*([^*\s][^*\n]*?)\*(?!\w)/g, "$1<em>$2</em>")
    .replace(/~~([^~\n]+)~~/g, "<del>$1</del>");
}

function link(url: string, textHtml: string): string {
  return `<a href="${url}" target="_blank" rel="noopener noreferrer">${textHtml}</a>`;
}

/** Inline spans of one text run (no code spans inside). */
function inlineText(raw: string): string {
  const stash: string[] = [];
  const hold = (html: string) => `\u0000${stash.push(html) - 1}\u0000`;
  let s = esc(raw);
  // [text](url) and bare URLs become placeholders first so emphasis never reaches an href.
  s = s.replace(/\[([^\]\n]+)\]\(([^\s)]+)\)|(https?:\/\/[^\s<]+[^\s<.,;:!?)\]'"])/g, (m, text, url, bare) => {
    if (bare) return hold(link(bare, bare));
    return SAFE_URL.test(url) ? hold(link(url, emphasis(text))) : m;
  });
  s = emphasis(s);
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => stash[Number(i)]);
}

export function inline(raw: string): string {
  return raw
    .split(/(`[^`\n]+`)/)
    .map((part, i) => (i % 2 === 1 ? `<code>${esc(part.slice(1, -1))}</code>` : inlineText(part)))
    .join("");
}

/** Row cells split on `|` — except an escaped `\|` or one inside a `code span`. */
function cells(line: string): string[] {
  const out: string[] = [];
  let cur = "";
  let code = false;
  const t = line.trim().replace(/^\|/, "");
  for (let i = 0; i < t.length; i++) {
    const ch = t[i];
    if (ch === "\\" && t[i + 1] === "|") {
      cur += "|";
      i++;
    } else if (ch === "`") {
      code = !code;
      cur += ch;
    } else if (ch === "|" && !code) {
      out.push(cur.trim());
      cur = "";
    } else cur += ch;
  }
  if (cur.trim() !== "" || !/\|\s*$/.test(t)) out.push(cur.trim());
  return out;
}
const isTableSep = (line: string) => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(line) && line.includes("-");
const LIST = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;

function task(text: string): string {
  return text.replace(/^\[ \]\s+/, "☐ ").replace(/^\[[xX]\]\s+/, "☑ ");
}

function tableHtml(head: string[] | null, align: string[], rows: string[][]): string {
  const n = Math.max(head?.length ?? 0, ...rows.map((r) => r.length));
  const pad = (r: string[]) => [...r, ...Array<string>(Math.max(0, n - r.length)).fill("")];
  const cell = (tag: string, c: string, i: number) => `<${tag} style="text-align:${align[i] ?? "left"}">${inline(c)}</${tag}>`;
  const tr = (tag: string, r: string[]) => `<tr>${pad(r).map((c, i) => cell(tag, c, i)).join("")}</tr>`;
  const thead = head ? `<thead>${tr("th", head)}</thead>` : "";
  return `<div class="chat-table"><table>${thead}<tbody>${rows.map((r) => tr("td", r)).join("")}</tbody></table></div>`;
}

function table(lines: string[]): string {
  const align = cells(lines[1]).map((c) => (c.endsWith(":") ? (c.startsWith(":") ? "center" : "right") : "left"));
  return tableHtml(cells(lines[0]), align, lines.slice(2).map(cells));
}

// ASCII/box-drawn tables (`+---+` / `┌─┬─┐`, rows between `|` or `│`) — agents print these as
// plain text; as a paragraph they lose alignment, so they render as a real table.
const ART_RULE = /^\s*[+┌├└╞╘╒┏┣┗╔╠╚][-=─━═+┬┼┴┐┤┘╡╪╛╕┓┫┛╗╣╝╦╬╩:\s]*[-─━═]{2}[-=─━═+┬┼┴┐┤┘╡╪╛╕┓┫┛╗╣╝╦╬╩:\s]*$/;
const ART_ROW = /^\s*[|│┃║].*[|│┃║]\s*$/;
const artCells = (line: string) =>
  line.trim().slice(1, -1).split(/[|│┃║]/).map((c) => c.trim());

/** Lines from `start` that form one drawn table, or 0 when they don't. */
function artTableLength(lines: string[], start: number): number {
  let i = start;
  let rows = 0;
  let rules = 0;
  for (; i < lines.length; i++) {
    if (ART_RULE.test(lines[i])) rules++;
    else if (ART_ROW.test(lines[i])) rows++;
    else break;
  }
  return rows >= 1 && rules >= 1 && rows + rules >= 3 ? i - start : 0;
}

function artTable(lines: string[]): string {
  // Inner rules separate rows; the outer frame doesn't count. Content before the first inner rule is the header.
  const body = lines.slice(ART_RULE.test(lines[0]) ? 1 : 0);
  while (body.length && ART_RULE.test(body[body.length - 1])) body.pop();
  const firstRule = body.findIndex((l) => ART_RULE.test(l));
  const rows = (ls: string[]) => ls.filter((l) => ART_ROW.test(l)).map(artCells);
  if (firstRule > 0 && body.slice(firstRule).some((l) => ART_ROW.test(l))) {
    const head = rows(body.slice(0, firstRule));
    // A header wrapped onto two lines is joined cell by cell.
    const merged = head.reduce((acc, r) => acc.map((c, i) => [c, r[i] ?? ""].filter(Boolean).join(" ")));
    return tableHtml(merged, [], rows(body.slice(firstRule)));
  }
  return tableHtml(null, [], rows(body));
}

/** Items of one list block → nested <ul>/<ol> by indentation. */
function list(items: { indent: number; ordered: boolean; text: string }[]): string {
  let out = "";
  const stack: { indent: number; tag: string }[] = [];
  const close = () => {
    const top = stack.pop()!;
    out += `</li></${top.tag}>`;
  };
  for (const it of items) {
    const tag = it.ordered ? "ol" : "ul";
    while (stack.length && it.indent < stack[stack.length - 1].indent) close();
    const top = stack[stack.length - 1];
    if (top && top.indent === it.indent) {
      if (top.tag === tag) out += "</li>";
      else {
        close();
        out += `<${tag}>`;
        stack.push({ indent: it.indent, tag });
      }
    } else {
      out += `<${tag}>`;
      stack.push({ indent: it.indent, tag });
    }
    out += `<li>${inline(task(it.text))}`;
  }
  while (stack.length) close();
  return out;
}

export function renderMarkdown(text: string): string {
  return markdown(text, 0);
}

function markdown(text: string, quoteDepth: number): string {
  // Transcript content is untrusted: keep recursion and DOM nesting bounded.
  if (quoteDepth >= 32) return `<p>${esc(text).replace(/\r\n?|\n/g, "<br>")}</p>`;
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  let out = "";
  let para: string[] = [];
  const flushPara = () => {
    if (para.length) out += `<p>${para.map(inline).join("<br>")}</p>`;
    para = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = line.match(/^\s*```+\s*([\w+#.-]*)\s*$/);
    if (fence) {
      flushPara();
      const body: string[] = [];
      for (i++; i < lines.length && !/^\s*```+\s*$/.test(lines[i]); i++) body.push(lines[i]);
      const lang = fence[1] ? ` data-lang="${esc(fence[1])}"` : "";
      out += `<pre${lang}><button type="button" class="chat-copy" data-copy>Kopyala</button><code>${esc(body.join("\n"))}</code></pre>`;
      continue;
    }
    if (line.trim() === "") {
      flushPara();
      continue;
    }
    const h = line.match(/^(#{1,6})\s+(.*?)\s*#*\s*$/);
    if (h) {
      flushPara();
      out += `<h${h[1].length}>${inline(h[2])}</h${h[1].length}>`;
      continue;
    }
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) {
      flushPara();
      out += "<hr>";
      continue;
    }
    if (line.includes("|") && i + 1 < lines.length && isTableSep(lines[i + 1]) && !isTableSep(line)) {
      flushPara();
      const rows = [line, lines[i + 1]];
      for (i += 2; i < lines.length && lines[i].includes("|") && lines[i].trim() !== ""; i++) rows.push(lines[i]);
      i--;
      out += table(rows);
      continue;
    }
    const art = artTableLength(lines, i);
    if (art) {
      flushPara();
      out += artTable(lines.slice(i, i + art));
      i += art - 1;
      continue;
    }
    if (/^\s*>/.test(line)) {
      flushPara();
      const q: string[] = [];
      for (; i < lines.length && /^\s*>/.test(lines[i]); i++) q.push(lines[i].replace(/^\s*>\s?/, ""));
      i--;
      out += `<blockquote>${markdown(q.join("\n"), quoteDepth + 1)}</blockquote>`;
      continue;
    }
    if (LIST.test(line)) {
      flushPara();
      const items: { indent: number; ordered: boolean; text: string }[] = [];
      for (; i < lines.length; i++) {
        const m = lines[i].match(LIST);
        if (!m) break;
        items.push({ indent: m[1].replace(/\t/g, "    ").length, ordered: /\d/.test(m[2]), text: m[3] });
      }
      i--;
      out += list(items);
      continue;
    }
    para.push(line);
  }
  flushPara();
  return out;
}
