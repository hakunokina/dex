import { defineConfig, devices } from "@playwright/test";
import { VITE_PORT } from "../../e2e/paths";

// README screenshots (docs/screenshots): `pnpm screenshots`. Not part of `pnpm test:e2e` (its testDir
// is e2e/). One worker, one demo dexd with made-up data; see shots.ts.
const viewport = { width: 1600, height: 900 };
export default defineConfig({
  testDir: ".",
  testMatch: "shots.ts",
  globalSetup: "./setup.ts",
  workers: 1,
  retries: 0,
  timeout: 180_000,
  reporter: [["list"]],
  outputDir: "../../.dex/tmp/shots-results",
  use: {
    baseURL: `http://127.0.0.1:${VITE_PORT}`,
    trace: "off",
    video: "off",
    screenshot: "off",
    colorScheme: "dark",
    locale: "tr-TR",
    timezoneId: "Europe/Istanbul",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport, deviceScaleFactor: 1 } }],
  webServer: {
    command: `pnpm exec vite --port ${VITE_PORT} --strictPort --host 127.0.0.1`,
    url: `http://127.0.0.1:${VITE_PORT}`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
