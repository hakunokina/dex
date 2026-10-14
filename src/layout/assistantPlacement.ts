/** Three columns use the existing 420 px worker-cell breakpoint. */
export const assistantDirection = (width: number): "right" | "below" => width >= 3 * 420 ? "right" : "below";
