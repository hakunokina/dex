import type { PtyHandlers as Sink } from "./api/transport";

export class PtyRelay {
  ended = false;
  private sink: Sink | null = null;
  private pending: ((sink: Sink) => void)[] = [];
  private size?: [number, number];
  private pendingSize = false;
  private buffer: boolean;

  constructor(buffer = true) { this.buffer = buffer; }

  get hasSink() { return this.sink !== null; }

  private deliver(item: (sink: Sink) => void) {
    if (this.sink) item(this.sink);
    else if (this.buffer) this.pending.push(item);
  }

  onData(bytes: Uint8Array) { this.deliver(sink => sink.onData(bytes)); }
  onExit(lost?: boolean) {
    this.ended = true;
    this.deliver(sink => sink.onExit(lost));
  }
  onSize(cols: number, rows: number) {
    this.size = [cols, rows];
    if (!this.sink && this.buffer) this.pendingSize = true;
    this.deliver(sink => sink.onSize?.(cols, rows));
  }
  onReattach() { this.deliver(sink => sink.onReattach?.()); }

  attach(sink: Sink) {
    this.sink = sink;
    const pending = this.pending;
    this.pending = [];
    if (!this.pendingSize && this.size) sink.onSize?.(...this.size);
    this.pendingSize = false;
    for (const item of pending) item(sink);
  }

  detach(sink: Sink) {
    if (this.sink === sink) this.sink = null;
  }
}
