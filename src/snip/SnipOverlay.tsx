// #34: one of the per-monitor selection overlays opened by `snip_start` (src-tauri/src/snip.rs).
// The window is transparent; the dim is the selection box's huge shadow (whole screen until a drag).
// The rect goes back in this window's CSS pixels with its devicePixelRatio — Rust maps it to
// physical pixels of the grab.
import { useEffect, useState } from "react";
import { TauriTransport } from "../api/transport";

const core = new TauriTransport();
type Rect = { x: number; y: number; w: number; h: number };

export default function SnipOverlay() {
  const [start, setStart] = useState<{ x: number; y: number } | null>(null);
  const [rect, setRect] = useState<Rect | null>(null);

  useEffect(() => {
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") void core.call("snip_cancel");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const down = (e: React.PointerEvent) => {
    if (e.button === 2) return void core.call("snip_cancel");
    if (e.button !== 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    setStart({ x: e.clientX, y: e.clientY });
    setRect({ x: e.clientX, y: e.clientY, w: 0, h: 0 });
  };
  const move = (e: React.PointerEvent) => {
    if (!start) return;
    setRect({
      x: Math.min(start.x, e.clientX),
      y: Math.min(start.y, e.clientY),
      w: Math.abs(e.clientX - start.x),
      h: Math.abs(e.clientY - start.y),
    });
  };
  const up = () => {
    setStart(null);
    // A plain click (no drag) keeps the overlay open, like the Windows tool.
    if (!rect || rect.w < 2 || rect.h < 2) return setRect(null);
    // devicePixelRatio, not the window's DPI scale: WebView2 also multiplies in Windows' text size.
    void core.call("snip_finish", { ...rect, dpr: window.devicePixelRatio }).catch((err) => console.error("snip", err));
  };

  return (
    <div
      onPointerDown={down}
      onPointerMove={move}
      onPointerUp={up}
      onContextMenu={(e) => e.preventDefault()}
      style={{ position: "fixed", inset: 0, cursor: "crosshair", overflow: "hidden", userSelect: "none" }}
    >
      <div
        style={
          rect && rect.w + rect.h > 0
            ? {
                position: "absolute",
                left: rect.x,
                top: rect.y,
                width: rect.w,
                height: rect.h,
                outline: "1px solid #fff",
                boxShadow: "0 0 0 100000px rgba(0,0,0,0.45)",
              }
            : { position: "absolute", inset: 0, background: "rgba(0,0,0,0.45)" }
        }
      />
    </div>
  );
}
