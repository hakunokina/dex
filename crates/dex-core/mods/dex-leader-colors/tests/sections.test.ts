import { expect, test } from 'claude-code/testing'
import { dexColor, splitDexBlocks, splitFences, splitSections } from '../hooks/sections.ts'

test('splits fenced code out of framed text, nested fences stay whole', () => {
  expect(splitFences('a\n  ```sh\nx\n  ```\nb\n````\n```\ny\n````\n')).toEqual([
    { code: false, text: 'a\n' },
    { code: true, text: '  ```sh\nx\n  ```\n' },
    { code: false, text: 'b\n' },
    { code: true, text: '````\n```\ny\n````\n' },
  ])
  expect(splitFences('düz metin')).toEqual([{ code: false, text: 'düz metin' }])
  expect(splitFences('```\nkapanmamış')).toEqual([{ code: true, text: '```\nkapanmamış' }])
})

test('keeps the preamble and groups exact line headings in order', () => {
  expect(splitSections('Önce **not**.\n\nSenden beklenen:\n- Karar\n\nSüren:\n- Test\nBiten:\n- Kod\n')).toEqual([
    { kind: 'markdown', text: 'Önce **not**.\n\n' },
    { kind: 'group', heading: 'Senden beklenen:', color: 'warning', text: '- Karar\n\n' },
    { kind: 'group', heading: 'Süren:', color: 'success', text: '- Test\n' },
    { kind: 'group', heading: 'Biten:', color: 'suggestion', text: '- Kod\n' },
  ])
})

test('does not group inline, quoted, indented, or fenced example headings', () => {
  const text = 'Bu satırda Biten:\n> Süren:\n    Senden beklenen:\n```md\nSüren:\n```\n~~~\nBiten:\n~~~\n'
  expect(splitSections(text)).toEqual([{ kind: 'markdown', text }])
})

test('preserves CRLF, repeated and empty groups, and incomplete streaming headings', () => {
  expect(splitSections('Süren:  \r\n- A\r\nSüren:\r\nBiten:')).toEqual([
    { kind: 'group', heading: 'Süren:', color: 'success', text: '- A\r\n' },
    { kind: 'group', heading: 'Süren:', color: 'success', text: '' },
    { kind: 'group', heading: 'Biten:', color: 'suggestion', text: '' },
  ])
  expect(splitSections('Senden bekle')).toEqual([{ kind: 'markdown', text: 'Senden bekle' }])
  expect(splitSections('')).toEqual([])
})

test('groups bold and markdown-heading forms under the plain heading', () => {
  expect(splitSections('Not.\n**Senden beklenen:**\n- Karar\n__Süren__:\n- Test\n## Biten:\n- Kod\n')).toEqual([
    { kind: 'markdown', text: 'Not.\n' },
    { kind: 'group', heading: 'Senden beklenen:', color: 'warning', text: '- Karar\n' },
    { kind: 'group', heading: 'Süren:', color: 'success', text: '- Test\n' },
    { kind: 'group', heading: 'Biten:', color: 'suggestion', text: '- Kod\n' },
  ])
  const text = '**Biten:** bitti\n*Süren:*\n'
  expect(splitSections(text)).toEqual([{ kind: 'markdown', text }])
})

test('dexColor paints per type, failures red and fixes orange', () => {
  expect(dexColor('Dex-Devretme', '[Dex-Devretme] K #1 · bitti — x')).toBe('#3fb950')
  expect(dexColor('Dex-Devretme', '[Dex-Devretme] K #1 · başarısız')).toBe('#f85149')
  expect(dexColor('Dex-Devretme', '[Dex-Devretme] K #1 · RAPORSUZ bitti')).toBe('#f85149')
  expect(dexColor('Dex-İnceleme', '[Dex-İnceleme] #2 · onay')).toBe('#3fb950')
  expect(dexColor('Dex-İnceleme', '[Dex-İnceleme] #2 · düzelt')).toBe('#d29922')
  expect(dexColor('Dex-Soru', '[Dex-Soru] K #1 · soru #2 — x')).toBe('#e3b341')
  expect(dexColor('Dex-Cevap', '[Dex-Cevap] x')).toBe('#3fb950')
  expect(dexColor('Dex-Plan', '[Dex-Plan] Plan #1 tamamlandı.')).toBe('#58a6ff')
  expect(dexColor('Dex-Üye', '[Dex-Üye] ONAYLANDI')).toBe('#bc8cff')
  expect(dexColor('Dex-Üye', '[Dex-Üye] REDDEDİLDİ')).toBe('#f85149')
  expect(dexColor('Dex-Not', '[Dex-Not] not')).toBe('#8b949e')
  expect(dexColor('Dex-Sistem', '[Dex-Sistem] kota')).toBe('#39c5cf')
  expect(dexColor('Dex-Motivasyon', '[Dex-Motivasyon] bugün')).toBe('#f778ba')
  expect(dexColor('Dex-Bilinmez', 'x')).toBe('#8b949e')
})

test('splitDexBlocks pulls typed lines out, fences stay verbatim', () => {
  expect(splitDexBlocks('[Dex-Devretme] Kaan #1 · bitti — iş\n  özet: detay\n- [Dex-Soru] K #1 · soru #2 — neden?\n')).toEqual([
    { kind: 'dex', tag: 'Dex-Devretme', text: '[Dex-Devretme] Kaan #1 · bitti — iş\n' },
    { kind: 'text', text: '  özet: detay\n' },
    { kind: 'dex', tag: 'Dex-Soru', text: '- [Dex-Soru] K #1 · soru #2 — neden?\n' },
  ])
  expect(splitDexBlocks('düz metin')).toEqual([{ kind: 'text', text: 'düz metin' }])
  expect(splitDexBlocks('```\n[Dex-Devretme] sahte\n```\n')).toEqual([{ kind: 'text', text: '```\n[Dex-Devretme] sahte\n```\n' }])
  expect(splitDexBlocks('[Dex] eski\n[Dexter] oyun\n')).toEqual([{ kind: 'text', text: '[Dex] eski\n[Dexter] oyun\n' }])
})

test('keeps nested fences and unclosed fences within their markdown section', () => {
  expect(splitSections('Biten:\n````md\n```\nSüren:\n````\nSenden beklenen:\n- Onay')).toEqual([
    { kind: 'group', heading: 'Biten:', color: 'suggestion', text: '````md\n```\nSüren:\n````\n' },
    { kind: 'group', heading: 'Senden beklenen:', color: 'warning', text: '- Onay' },
  ])
  const text = '~~~md\nBiten:\n'
  expect(splitSections(text)).toEqual([{ kind: 'markdown', text }])
})
