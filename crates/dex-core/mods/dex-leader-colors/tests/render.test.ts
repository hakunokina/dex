import { expect, test } from 'claude-code/testing'

const OTHER = '#a47148'

for (const surface of ['terminal', 'desktop', 'vscode', 'mobile'] as const) {
  test(`draws framed colored groups and a brown "other" frame on ${surface}`, async ($, on) => {
    on('ui.render', () => ({ type: 'Text', props: {}, children: ['default'] }))
    const ui = await $.ui.mount({
      plugin: 'dex-leader-colors', surface, component: 'AssistantMessage',
      props: { text: 'Ön söz.\nSenden beklenen:\n- Karar\nSüren:\n- Test\nBiten:\n- Kod', isFirstOfReply: true },
    })
    expect(await ui.drawn()).toMatchObject({
      type: 'Box', props: { flexDirection: 'column' }, children: [
        { type: 'Box', children: [
          { type: 'Box', props: { borderStyle: 'single', borderColor: OTHER }, children: [
            { type: 'Markdown', props: { text: 'Ön söz.\n' } },
          ] },
        ] },
        { type: 'Box', children: [
          { type: 'Text', props: { bold: true, color: 'warning' }, children: ['Senden beklenen:'] },
          { type: 'Box', props: { borderStyle: 'single', borderColor: 'warning' }, children: [
            { type: 'Markdown', props: { text: '- Karar\n' } },
          ] },
        ] },
        { type: 'Box', children: [
          { type: 'Text', props: { bold: true, color: 'success' }, children: ['Süren:'] },
          { type: 'Box', props: { borderStyle: 'single', borderColor: 'success' } },
        ] },
        { type: 'Box', children: [
          { type: 'Text', props: { bold: true, color: 'suggestion' }, children: ['Biten:'] },
          { type: 'Box', props: { borderStyle: 'single', borderColor: 'suggestion' } },
        ] },
      ],
    })
  })

  test(`frames an ordinary message in brown on ${surface}`, async ($, on) => {
    on('ui.render', () => ({ type: 'Text', props: {}, children: ['default'] }))
    const ui = await $.ui.mount({
      plugin: 'dex-leader-colors', surface, component: 'AssistantMessage',
      props: { text: 'Normal **yanıt**.', isFirstOfReply: false },
    })
    expect(await ui.drawn()).toMatchObject({
      type: 'Box', children: [
        { type: 'Box', children: [
          { type: 'Box', props: { borderStyle: 'single', borderColor: OTHER }, children: [
            { type: 'Markdown', props: { text: 'Normal **yanıt**.' } },
          ] },
        ] },
      ],
    })
  })

  test(`draws code blocks outside the frame on ${surface}`, async ($, on) => {
    on('ui.render', () => ({ type: 'Text', props: {}, children: ['default'] }))
    const ui = await $.ui.mount({
      plugin: 'dex-leader-colors', surface, component: 'AssistantMessage',
      props: { text: 'Senden beklenen:\n- Sil:\n```\nrm -rf x\n```\n- Sonra bak.\n', isFirstOfReply: true },
    })
    expect(await ui.drawn()).toMatchObject({
      type: 'Box', children: [
        { type: 'Box', children: [
          { type: 'Text', children: ['Senden beklenen:'] },
          { type: 'Box', props: { borderStyle: 'single', borderColor: 'warning' }, children: [{ type: 'Markdown', props: { text: '- Sil:\n' } }] },
          { type: 'Markdown', props: { text: '```\nrm -rf x\n```\n' } },
          { type: 'Box', props: { borderStyle: 'single', borderColor: 'warning' }, children: [{ type: 'Markdown', props: { text: '- Sonra bak.\n' } }] },
        ] },
      ],
    })
  })

  test(`passes summaries and blank text unchanged on ${surface}`, async ($, on) => {
    const propsSeen: unknown[] = []
    on('ui.render', ($, e) => {
      propsSeen.push(e.props)
      return { type: 'Text', props: {}, children: ['default'] }
    })
    for (const props of [
      { text: 'Biten:\n- Özet', isFirstOfReply: true, isSummary: true as const },
      { text: '  \n', isFirstOfReply: false },
    ]) {
      const ui = await $.ui.mount({ plugin: 'dex-leader-colors', surface, component: 'AssistantMessage', props })
      expect(await ui.drawn()).toEqual({ type: 'Text', props: {}, children: ['default'] })
      expect(propsSeen[propsSeen.length - 1]).toEqual(props)
    }
  })

  test(`paints dex lines as plain colored text in the user message on ${surface}`, async ($, on) => {
    on('ui.render', () => ({ type: 'Text', props: {}, children: ['default'] }))
    const ui = await $.ui.mount({
      plugin: 'dex-leader-colors', surface, component: 'UserMessage',
      props: { text: '[Dex-Devretme] Kaan #1 · bitti — iş\n  özet: detay\n[Dex-Soru] Kaan #1 · soru #2 — neden?' },
    })
    expect(await ui.drawn()).toMatchObject({
      type: 'Box', props: { flexDirection: 'column' }, children: [
        { type: 'Text', props: { bold: true, color: '#3fb950' }, children: ['[Dex-Devretme] Kaan #1 · bitti — iş'] },
        { type: 'Markdown', props: { text: '  özet: detay\n' } },
        { type: 'Text', props: { bold: true, color: '#e3b341' }, children: ['[Dex-Soru] Kaan #1 · soru #2 — neden?'] },
      ],
    })
  })

  test(`passes a dex-free user message unchanged on ${surface}`, async ($, on) => {
    on('ui.render', () => ({ type: 'Text', props: {}, children: ['default'] }))
    const ui = await $.ui.mount({
      plugin: 'dex-leader-colors', surface, component: 'UserMessage',
      props: { text: 'Normal **yanıt**.' },
    })
    expect(await ui.drawn()).toMatchObject({ type: 'Text', props: {}, children: ['default'] })
  })

  test(`paints dex lines unframed beside framed groups in the reply on ${surface}`, async ($, on) => {
    on('ui.render', () => ({ type: 'Text', props: {}, children: ['default'] }))
    const ui = await $.ui.mount({
      plugin: 'dex-leader-colors', surface, component: 'AssistantMessage',
      props: { text: '[Dex-Devretme] Kaan #1 · başarısız\nSüren:\n- Test', isFirstOfReply: true },
    })
    expect(await ui.drawn()).toMatchObject({
      type: 'Box', children: [
        { type: 'Text', props: { bold: true, color: '#f85149' }, children: ['[Dex-Devretme] Kaan #1 · başarısız'] },
        { type: 'Box', children: [
          { type: 'Box', children: [
            { type: 'Text', props: { bold: true, color: 'success' }, children: ['Süren:'] },
            { type: 'Box', props: { borderStyle: 'single', borderColor: 'success' } },
          ] },
        ] },
      ],
    })
  })
}
