// A remote dexd can be older than this client (#94 added `disabled`): fields it does not send are empty, never undefined.
import type { ChainView } from "./api";

export function chainView(raw: Partial<ChainView> | null | undefined): ChainView {
  return {
    order: Array.isArray(raw?.order) ? raw.order : [],
    disabled: Array.isArray(raw?.disabled) ? raw.disabled : [],
    exhausted: Array.isArray(raw?.exhausted) ? raw.exhausted : [],
  };
}
