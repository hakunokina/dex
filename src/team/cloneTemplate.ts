import type { ProviderInfo, TeamCloneTemplate } from "./api";
import { providerModels } from "./providerModels.ts";

const foreign = (engine: string | null | undefined) => engine != null && engine !== "claude";

/** Old hosts may still return Codex (or unknown-engine) members; the form can no longer pick an engine,
 *  so they become Anthropic Claude members instead of being sent back silently. `converted` lists their indexes. */
export function normalizeCloneTemplate(t: TeamCloneTemplate, providers: ProviderInfo[]): { template: TeamCloneTemplate; converted: number[] } {
  const converted: number[] = [];
  const model = providerModels(providers, "anthropic")[0]?.id ?? "";
  const members = t.members.map((m, i) => {
    if (!foreign(m.engine)) return m;
    converted.push(i);
    return { ...m, engine: null, provider: "anthropic", model };
  });
  return { template: converted.length ? { ...t, members } : t, converted };
}

/** Converted members got a fallback model before the catalog arrived; move them to the catalog's
 *  first Anthropic model when their model is not in it (and they are still on Anthropic). */
export function fitConvertedModels(t: TeamCloneTemplate, converted: number[], providers: ProviderInfo[]): TeamCloneTemplate {
  const list = providerModels(providers, "anthropic");
  const first = list[0]?.id;
  if (!first) return t;
  let changed = false;
  const members = t.members.map((m, i) => {
    if (!converted.includes(i) || (m.provider ?? "anthropic") !== "anthropic" || list.some(x => x.id === m.model)) return m;
    changed = true;
    return { ...m, model: first };
  });
  return changed ? { ...t, members } : t;
}
