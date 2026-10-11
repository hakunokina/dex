// Kademe 1 Hafıza Haritası'nın veri kapısı (crates/dex-core/src/memory.rs).
// Aynı kapıdan: `callHost` + `splitId` (src/api/hosts.ts). Takım id'si niteliklidir
// (`<host>:<id>`); host her zaman kendi çıplak id'siyle çağrılır.
import { callHost, hostSupports, qOpt, splitId } from "../api/hosts";
import { memoryRequest } from "../project/model";

export interface MemNode {
  /** `"<scope>/<name>"`, ör. `"shared/pty-size-owner"`, `"agents/timur/open-items"`. */
  id: string;
  name: string;
  scope: "shared" | "agent" | "missing" | "team" | "project" | "private";
  agentId: string | null;
  description: string;
  /** `metadata.type`. */
  type: string | null;
  /** Ham `[[slug]]` listesi. */
  links: string[];
  /** ms. */
  updatedAt: number;
  size: number;
}

export interface MemEdge {
  from: string;
  to: string;
}

export interface MemGraph {
  nodes: MemNode[];
  edges: MemEdge[];
}

export interface MemDoc {
  id: string;
  frontmatter: Record<string, string>;
  /** Frontmatter sonrası markdown. */
  body: string;
}

/** Bu host'un dexd'si `memory_*` bilmiyor (istemciden eski) — harita gösterilemez. */
export class MemoryUnsupportedError extends Error {
  readonly host: string;
  constructor(host: string, method: string) {
    super(`${host}: bu sunucu hafıza haritasını desteklemiyor (eski dexd, metot yok: ${method})`);
    this.name = "MemoryUnsupportedError";
    this.host = host;
  }
}

/**
 * #142: yeni dexd metot listesini `auth.hello`'da verir — eskisi hiç çağrılmadan anlaşılır.
 * Listesiz (#142 öncesi) dexd için aşağıdaki metin eşleşmesi yedek yol olarak kalır.
 */
function unsupported(host: string, method: string): Promise<never> | null {
  return hostSupports(host, method) ? null : Promise.reject(new MemoryUnsupportedError(host, method));
}

/**
 * dexd bilmediği metoda `unknown_method` ile cevap verir; transports'tan yalnız
 * hata metni geçer (crates/dexd/src/methods.rs, ilk dexd'den beri değişmedi).
 */
const isUnknownMethod = (err: unknown, method: string) =>
  (err instanceof Error ? err.message : String(err)) === `bilinmeyen metot: ${method}`;

export function memoryGraph(teamId: string, projectId: string | null = null, legacy = true): Promise<MemGraph> {
  const [host, raw] = splitId(teamId);
  if (projectId && splitId(projectId)[0] !== host) return Promise.reject(new Error("project_mismatch"));
  const { method, params } = memoryRequest("graph", raw, projectId ? splitId(projectId)[1] : null, legacy);
  return unsupported(host, method) ?? callHost<MemGraph>(host, method, params).then(g => ({
    ...g, nodes: g.nodes.map(n => ({ ...n, agentId: qOpt(host, n.agentId) })),
  })).catch((err) => {
    if (isUnknownMethod(err, method)) throw new MemoryUnsupportedError(host, method);
    throw err;
  });
}

export function memoryRead(teamId: string, id: string, projectId: string | null = null, legacy = true): Promise<MemDoc> {
  const [host, raw] = splitId(teamId);
  if (projectId && splitId(projectId)[0] !== host) return Promise.reject(new Error("project_mismatch"));
  const { method, params } = memoryRequest("read", raw, projectId ? splitId(projectId)[1] : null, legacy, id);
  return unsupported(host, method) ?? callHost<MemDoc>(host, method, params).catch((err) => {
    if (isUnknownMethod(err, method)) throw new MemoryUnsupportedError(host, method);
    throw err;
  });
}
