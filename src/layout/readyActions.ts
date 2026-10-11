export class ReadyActions {
  private pending = new Map<string, (() => void)[]>();

  run(teamId: string, ready: boolean, action: () => void) {
    if (ready) return action();
    const actions = this.pending.get(teamId) ?? [];
    actions.push(action);
    this.pending.set(teamId, actions);
  }

  flush(teamId: string) {
    const actions = this.pending.get(teamId) ?? [];
    this.pending.delete(teamId);
    for (const action of actions) action();
  }
}
