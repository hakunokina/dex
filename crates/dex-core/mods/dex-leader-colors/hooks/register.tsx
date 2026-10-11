import type { Register } from 'claude-code'
import { splitFences, splitSections } from './sections.ts'

/** "Diğer": text outside the three groups, coffee brown (patron). */
export const OTHER = '#a47148'

export const register: Register = on => {
  on('ui.render', { component: 'AssistantMessage' }, ($, e, next) => {
    if (e.props.isSummary) return next(e)
    const sections = splitSections(e.props.text).filter(s => s.kind === 'group' || s.text.trim())
    if (!sections.length) return next(e)
    const { Box, Text, Markdown } = $.ui.resolve(e)
    if (!Box || !Text || !Markdown) return next(e)
    // Code blocks are drawn outside the frame: copying a command must not pick up its border.
    const framed = (text: string, color: string) => splitFences(text).map(part => part.code
      ? <Markdown text={part.text} />
      : <Box borderStyle="single" borderColor={color} paddingX={1}>
          <Markdown text={part.text} />
        </Box>)
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
  })
}
