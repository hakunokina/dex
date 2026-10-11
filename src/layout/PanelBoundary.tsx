// A panel that throws while rendering must not take the whole window with it: the dock restores the
// same panels at every start, so an unguarded throw (#103) would blank Dex on every launch.
import { Component, type ReactNode } from "react";

interface State {
  error: unknown;
}

export class PanelBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: unknown): State {
    return { error: error ?? new Error("bilinmeyen hata") };
  }

  componentDidCatch(error: unknown) {
    console.error("panel çöktü:", error);
  }

  render() {
    if (this.state.error === null) return this.props.children;
    const text = this.state.error instanceof Error ? this.state.error.message : String(this.state.error);
    return (
      <div className="panel-fill" role="alert" style={{ padding: 12 }}>
        <p className="settings-err">Bu panel açılamadı: {text}</p>
        <button onClick={() => this.setState({ error: null })}>Tekrar dene</button>
      </div>
    );
  }
}
