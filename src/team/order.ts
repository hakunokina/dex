// Görev #81: display order of a team's members — leaders, then role groups in catalog order, then
// names in natural order inside a group (Pixel-1 < Pixel-2 < Pixel-10; unnumbered names last).
// Only the order on screen; ids and the DB `sort` column are untouched.

export interface Orderable {
  name: string;
  kind: string;
  roleSlug: string | null;
}

const collator = new Intl.Collator("tr", { numeric: true, sensitivity: "base" });
const numbered = (name: string) => /\d+$/.test(name);

/** `roleOrder`: catalog slugs in catalog order; roles outside it (or an empty catalog) go alphabetical. */
export function sortAgents<T extends Orderable>(agents: T[], roleOrder: string[]): T[] {
  const rank = new Map(roleOrder.map((slug, i) => [slug, i]));
  const roleRank = (slug: string | null) => (slug === null ? Infinity : rank.get(slug) ?? roleOrder.length);
  return [...agents].sort((a, b) =>
    Number(b.kind === "leader") - Number(a.kind === "leader") ||
    (a.kind === "leader" ? 0 : roleRank(a.roleSlug) - roleRank(b.roleSlug) || collator.compare(a.roleSlug ?? "", b.roleSlug ?? "")) ||
    Number(numbered(b.name)) - Number(numbered(a.name)) ||
    collator.compare(a.name, b.name));
}
