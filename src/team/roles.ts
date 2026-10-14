// #1720: the role catalog comes from the local core. Loaded once, it went stale when the app was
// opened against an exiting core during an update (new roles hidden until restart) — so a local
// reconnect fetches it again, apart from the teams reload (whose failure must not drop new roles).
// Every fetch takes a generation: only the newest one writes, so a late old answer never overwrites
// a newer list. If the newest one fails, `stale` stays set and the next reload tries again.

export interface RoleSync {
  /** The newest fetch failed: the list on screen may be old. */
  readonly stale: boolean;
  /** Fetch and, if no newer fetch started meanwhile, write. Rejects when the fetch fails. */
  refresh(): Promise<void>;
}

export function roleSync<R>(fetch: () => Promise<R[]>, write: (roles: R[]) => void): RoleSync {
  let gen = 0;
  let stale = false;
  return {
    get stale() {
      return stale;
    },
    async refresh() {
      const mine = ++gen;
      let roles: R[];
      try {
        roles = await fetch();
      } catch (e) {
        if (mine === gen) stale = true;
        throw e;
      }
      if (mine !== gen) return;
      stale = false;
      write(roles);
    },
  };
}
