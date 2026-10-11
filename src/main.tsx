import React, { lazy, Suspense } from "react";
import ReactDOM from "react-dom/client";
// DEX-37: fonts are bundled (offline desktop app, no CDN).
import "@fontsource-variable/inter/opsz.css";
import "@fontsource-variable/jetbrains-mono/wght.css";
// S5-A: one shell per platform — the desktop workspace (dockview, panes, topbar) never loads in the
// mobile bundle, and the mobile shell knows nothing of docks. Same fonts, same tokens.
import { MOBILE } from "./api/platform";
import { applyDensity } from "./density";

// #34: `?snip` = a screenshot selection overlay window (src-tauri/src/snip.rs), not the workspace.
const SNIP = new URLSearchParams(location.search).has("snip");
const Root = SNIP
  ? lazy(() => import("./snip/SnipOverlay"))
  : MOBILE
    ? lazy(() => import("./mobile/MobileApp"))
    : lazy(() => import("./App"));

// #33: desktop density tokens before anything measures (the phone shell keeps its touch sizes).
if (!MOBILE && !SNIP) applyDensity();

// xterm measures the cell once at open — the mono face must be ready before any terminal mounts.
const fonts = Promise.all([
  document.fonts.load('400 13px "JetBrains Mono Variable"', "aığş"),
  document.fonts.load('600 13px "JetBrains Mono Variable"', "aığş"),
  document.fonts.load('400 13px "Inter Variable"', "aığş"),
]).catch(() => {});

void Promise.race([fonts, new Promise((r) => setTimeout(r, 1500))]).then(() =>
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <Suspense>
        <Root />
      </Suspense>
    </React.StrictMode>,
  ),
);
