import { defineConfig, devices } from "@playwright/test";
import { VITE_PORT } from "./e2e/paths";

// UI e2e: the desktop shell in Chromium, against a real dexd per worker (e2e/fixtures.ts).
// `pnpm test:e2e`. Video/trace are off; turn trace on for one failing spec when you need it.
export default defineConfig({
  testDir: "e2e",
  globalSetup: "./e2e/global-setup.ts",
  workers: 2,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  timeout: 60_000,
  reporter: [["list"]],
  outputDir: "test-results",
  use: {
    baseURL: `http://127.0.0.1:${VITE_PORT}`,
    trace: "off",
    video: "off",
    screenshot: "off",
    viewport: { width: 1400, height: 900 },
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1400, height: 900 } } }],
  webServer: {
    command: `pnpm exec vite --port ${VITE_PORT} --strictPort --host 127.0.0.1`,
    url: `http://127.0.0.1:${VITE_PORT}`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
