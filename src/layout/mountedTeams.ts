/** Keep visited teams until deletion; never construct an unopened workspace. */
export function mountedTeams(previous: readonly string[], existing: readonly string[], active: string): readonly string[] {
  const known = new Set(existing);
  const next = previous.filter(id => known.has(id));
  if (known.has(active) && !next.includes(active)) next.push(active);
  return next.length === previous.length && next.every((id, i) => id === previous[i]) ? previous : next;
}
