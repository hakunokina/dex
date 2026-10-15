export type Section =
  | { kind: 'markdown'; text: string }
  | { kind: 'group'; heading: string; color: 'warning' | 'success' | 'suggestion'; text: string }

/** A `[Dex-Tür]` line outside code fences: drawn as plain colored text, never framed. */
export type RichBlock =
  | { kind: 'dex'; tag: string; text: string }
  | { kind: 'text'; text: string }

/** Tür başına yazı rengi (kutu/çerçeve yok). Devretme ve İnceleme satırın durumuna göre değişir. */
export function dexColor(tag: string, line: string): string {
  switch (tag) {
    case 'Dex-Devretme': return /başarısız|RAPORSUZ/.test(line) ? '#f85149' : '#3fb950'
    case 'Dex-İnceleme': return /düzelt|REDDEDİLDİ|bekliyor/.test(line) ? '#d29922' : '#3fb950'
    case 'Dex-Soru': return '#e3b341'
    case 'Dex-Cevap': return '#3fb950'
    case 'Dex-Plan': return '#58a6ff'
    case 'Dex-Üye': return /REDDEDİLDİ/.test(line) ? '#f85149' : '#bc8cff'
    case 'Dex-Not': return '#8b949e'
    case 'Dex-Sistem': return '#39c5cf'
    case 'Dex-Motivasyon': return '#f778ba'
    default: return '#8b949e'
  }
}

const DEX_LINE = /^[ \t]*(?:-[ \t]+)?\[Dex-([A-Za-zÇĞİÖŞÜçğıöşü]+)\]/u

/** Fence-aware split: dex lines out, everything else kept verbatim (fences intact). */
export function splitDexBlocks(text: string): RichBlock[] {
  const blocks: RichBlock[] = []
  let current = ''
  let fence: string | undefined
  const flush = () => { if (current) { blocks.push({ kind: 'text', text: current }); current = '' } }
  for (const match of text.matchAll(/[^\n]*(?:\n|$)/g)) {
    const line = match[0]
    if (!line) continue
    const marker = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line.replace(/\r?\n$/, '').trimEnd())
    const delimiter = marker?.[1]
    if (fence) {
      current += line
      if (delimiter && delimiter[0] === fence[0] && delimiter.length >= fence.length && !marker?.[2]?.trim()) {
        fence = undefined
      }
      continue
    }
    if (delimiter && (delimiter[0] !== '`' || !marker?.[2]?.includes('`'))) {
      fence = delimiter
      current += line
      continue
    }
    const tag = DEX_LINE.exec(line.replace(/\r?\n$/, ''))?.[1]
    if (tag) {
      flush()
      blocks.push({ kind: 'dex', tag: `Dex-${tag}`, text: line })
    } else {
      current += line
    }
  }
  flush()
  return blocks
}

/** Fenced code blocks stand apart so the frame never runs beside a copyable command. */
export function splitFences(text: string): { code: boolean; text: string }[] {
  const parts: { code: boolean; text: string }[] = []
  let current = { code: false, text: '' }
  let fence: string | undefined
  const push = () => { if (current.text.trim()) parts.push(current) }
  for (const match of text.matchAll(/[^\n]*(?:\n|$)/g)) {
    const line = match[0]
    if (!line) continue
    const marker = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line.replace(/\r?\n$/, '').trimEnd())
    const delimiter = marker?.[1]
    if (fence) {
      current.text += line
      if (delimiter && delimiter[0] === fence[0] && delimiter.length >= fence.length && !marker?.[2]?.trim()) {
        fence = undefined
        push()
        current = { code: false, text: '' }
      }
      continue
    }
    if (delimiter && (delimiter[0] !== '`' || !marker?.[2]?.includes('`'))) {
      push()
      fence = delimiter
      current = { code: true, text: line }
      continue
    }
    current.text += line
  }
  push()
  return parts
}

export function splitSections(text: string): Section[] {
  const sections: Section[] = []
  let current: Section = { kind: 'markdown', text: '' }
  let fence: string | undefined
  for (const match of text.matchAll(/[^\n]*(?:\n|$)/g)) {
    const line = match[0]
    if (!line) continue
    const heading = line.replace(/\r?\n$/, '').trimEnd()
    const marker = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(heading)
    const delimiter = marker?.[1]
    if (fence) {
      current.text += line
      if (delimiter && delimiter[0] === fence[0] && delimiter.length >= fence.length && !marker?.[2]?.trim()) {
        fence = undefined
      }
      continue
    }
    if (delimiter && (delimiter[0] !== '`' || !marker?.[2]?.includes('`'))) {
      fence = delimiter
      current.text += line
      continue
    }
    // Leaders also write the heading as **X:**, **X**: or ## X: — same group.
    const bare = heading.replace(/^#{1,6} +/, '').replace(/^(\*\*|__)(.+?)\1(:?)$/, '$2$3')
    const color = bare === 'Senden beklenen:' ? 'warning'
      : bare === 'Süren:' ? 'success'
        : bare === 'Biten:' ? 'suggestion' : undefined
    if (color) {
      if (current.kind === 'group' || current.text) sections.push(current)
      current = { kind: 'group', heading: bare, color, text: '' }
    } else {
      current.text += line
    }
  }
  if (current.kind === 'group' || current.text) sections.push(current)
  return sections
}
