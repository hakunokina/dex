/** Three columns use the existing 420 px worker-cell breakpoint. */
export const assistantDirection = (width: number): "right" | "below" => width >= 3 * 420 ? "right" : "below";

/** Dex opens the assistant's pane by itself only for an assistant it hasn't opened yet; a closed one stays closed. */
export const opensAssistant = (assistantId: string, offered: string | undefined) => assistantId !== offered;
