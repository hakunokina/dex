interface ViewingState { teamId: string; viewed: boolean; connected: boolean }

export function createTeamViewing(
  read: () => ViewingState,
  send: (teamId: string, viewed: boolean) => Promise<void>,
  onError: () => void,
) {
  let last: ViewingState | null = null;
  let disposed = false;
  const pending = new Map<string, ViewingState>();
  const failed = new Map<string, ViewingState>();
  const publish = (state: ViewingState) => {
    failed.delete(state.teamId);
    pending.set(state.teamId, state);
    void send(state.teamId, state.viewed).catch(() => {
      // A late error must not replace a newer visibility event for this team.
      if (pending.get(state.teamId) === state) failed.set(state.teamId, state);
      onError();
    }).finally(() => {
      if (pending.get(state.teamId) === state) pending.delete(state.teamId);
    });
  };
  return {
    sync() {
      if (disposed) return;
      const next = read();
      if (last && last.teamId !== next.teamId && last.viewed) publish({ ...last, viewed: false });
      if (!next.teamId || !next.connected) {
        last = null;
        failed.delete(next.teamId);
        pending.delete(next.teamId);
      }
      else if (last?.teamId !== next.teamId || last.viewed !== next.viewed) {
        last = next;
        publish(next);
      }
      // Retry only on the next real event, including a previous team's failed false.
      for (const state of failed.values()) publish(state);
    },
    dispose() {
      disposed = true;
      if (last?.viewed) publish({ ...last, viewed: false });
      last = null;
      failed.clear();
      pending.clear();
    },
  };
}
