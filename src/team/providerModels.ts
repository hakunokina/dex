import type { ProviderInfo, ProviderModel } from "./api";

const DEFAULT_PROVIDER_MODELS: ProviderModel[] = [
  { id: "claude-fable-5-1", label: "Fable 5.1" }, { id: "claude-opus-5-5", label: "Opus 5.5" },
  { id: "claude-sonnet-5-5", label: "Sonnet 5.5" }, { id: "claude-haiku-4-5-20251001", label: "Haiku 4.5" },
];

export function providerModels(providers: ProviderInfo[], id: string | null | undefined, current?: string): ProviderModel[] {
  const p = providers.find(x => x.id === (id || "anthropic"));
  // Live catalogs never fall back to Anthropic, even before the provider list arrives.
  const all = p?.models.length || id === "local" || id === "gateway" ? p?.models ?? [] : DEFAULT_PROVIDER_MODELS;
  const list = all.filter(m => !m.legacy || m.id === current);
  return current && !list.some(m => m.id === current) ? [...list, { id: current, label: `${current} (listede yok)` }] : list;
}
