/** PTY events may arrive before the attach RPC's snapshot; replay must paint first. */
export class PtySnapshotQueue {
  private pending: (() => void)[] | null = [];

  deliver(item: () => void) {
    if (this.pending) this.pending.push(item);
    else item();
  }

  flush(snapshot: () => void) {
    snapshot();
    const pending = this.pending ?? [];
    this.pending = null;
    for (const item of pending) item();
  }
}
