import { expect, test } from 'claude-code/testing'
import { splitFences, splitSections } from '../hooks/sections.ts'

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

test('keeps nested fences and unclosed fences within their markdown section', () => {
  expect(splitSections('Biten:\n````md\n```\nSüren:\n````\nSenden beklenen:\n- Onay')).toEqual([
    { kind: 'group', heading: 'Biten:', color: 'suggestion', text: '````md\n```\nSüren:\n````\n' },
    { kind: 'group', heading: 'Senden beklenen:', color: 'warning', text: '- Onay' },
  ])
  const text = '~~~md\nBiten:\n'
  expect(splitSections(text)).toEqual([{ kind: 'markdown', text }])
})
