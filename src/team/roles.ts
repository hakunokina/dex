// #1720: the role catalog comes from the local core. Loaded once, it went stale when the app was
// opened against an exiting core during an update (new roles hidden until restart) — so a local
// reconnect marks it stale and the next reload fetches it again. A failed refetch keeps the old list.

export async function loadRoles<R>(prev: R[], stale: boolean, fetch: () => Promise<R[]>, localDown: () => boolean): Promise<R[]> {
  if (prev.length && !stale) return prev;
  try {
    return await fetch();
  } catch (e) {
    if (prev.length || localDown()) return prev;
    throw e;
  }
}
