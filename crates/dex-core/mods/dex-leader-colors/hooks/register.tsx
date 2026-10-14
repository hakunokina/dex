import type { Register } from 'claude-code'
import { dexColor, splitDexBlocks, splitFences, splitSections } from './sections.ts'

/** "Diğer": text outside the three groups, coffee brown (patron). */
export const OTHER = '#a47148'

/** [Dex-Tür] satırı: kutusuz, kalın ve tür renginde düz yazı. */
const dexLine = (Text: any, tag: string, text: string) => (
  <Text bold color={dexColor(tag, text)}>{text.replace(/\n$/, '')}</Text>
)

/** Dex satırı içermeyen metnin mevcut kutulu düzeni (başlık + çerçeveli gövde). */
const framedSections = ({ Box, Text, Markdown }: any, text: string) => {
  // Code blocks are drawn outside the frame: copying a command must not pick up its border.
  const framed = (body: string, color: string) => splitFences(body).map(part => part.code
    ? <Markdown text={part.text} />
    : <Box borderStyle="single" borderColor={color} paddingX={1}>
        <Markdown text={part.text} />
      </Box>)
  const sections = splitSections(text).filter(s => s.kind === 'group' || s.text.trim())
  return (
    <Box flexDirection="column">
      {sections.map(section => section.kind === 'markdown'
        ? <Box flexDirection="column">{framed(section.text, OTHER)}</Box>
        : <Box flexDirection="column">
            <Text bold color={section.color}>{section.heading}</Text>
            {framed(section.text, section.color)}
          </Box>)}
    </Box>
  )
}

export const register: Register = on => {
  // Dex paste'leri (panoya yapışan kullanıcı mesajı): [Dex-Tür] satırları düz yazı
  // rengi alır, gerisi aynen Markdown çizilir. Kutu/çerçeve yok.
  on('ui.render', { component: 'UserMessage' }, ($, e, next) => {
    const blocks = splitDexBlocks(e.props.text)
    if (!blocks.some(b => b.kind === 'dex')) return next(e)
    const { Box, Text, Markdown } = $.ui.resolve(e)
    if (!Box || !Text || !Markdown) return next(e)
    return (
      <Box flexDirection="column">
        {blocks.filter(block => block.kind === 'dex' || block.text.trim()).map(block => block.kind === 'dex'
          ? dexLine(Text, block.tag, block.text)
          : <Markdown text={block.text} />)}
      </Box>
    )
  })
  on('ui.render', { component: 'AssistantMessage' }, ($, e, next) => {
    if (e.props.isSummary) return next(e)
    const { Box, Text, Markdown } = $.ui.resolve(e)
    if (!Box || !Text || !Markdown) return next(e)
    if (!splitDexBlocks(e.props.text).some(b => b.kind === 'dex')) {
      const sections = splitSections(e.props.text).filter(s => s.kind === 'group' || s.text.trim())
      if (!sections.length) return next(e)
      return framedSections({ Box, Text, Markdown }, e.props.text)
    }
    return (
      <Box flexDirection="column">
        {splitDexBlocks(e.props.text).filter(block => block.kind === 'dex' || block.text.trim()).map(block => block.kind === 'dex'
          ? dexLine(Text, block.tag, block.text)
          : framedSections({ Box, Text, Markdown }, block.text))}
      </Box>
    )
  })
}
