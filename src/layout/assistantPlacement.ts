/** Dex opens the assistant's pane by itself only for an assistant it hasn't opened yet; a closed one stays closed. */
export const opensAssistant = (assistantId: string, offered: string | undefined) => assistantId !== offered;
