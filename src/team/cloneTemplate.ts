import type { ProviderInfo, TeamCloneTemplate } from "./api";
import { providerModels } from "./providerModels.ts";

/** Old hosts may still return Codex (or unknown-engine) members; the form can no longer pick an engine,
 *  so they become Anthropic Claude members instead of being sent back silently. */
export function normalizeCloneTemplate(t: TeamCloneTemplate, providers: ProviderInfo[]): { template: TeamCloneTemplate; converted: number } {
  let converted = 0;
  const model = providerModels(providers, "anthropic")[0]?.id ?? "";
  const members = t.members.map(m => {
    if (m.engine == null || m.engine === "claude") return m;
    converted++;
    return { ...m, engine: null, provider: "anthropic", model };
  });
  return { template: converted ? { ...t, members } : t, converted };
}
