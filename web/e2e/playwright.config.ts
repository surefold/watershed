import { fileURLToPath } from "node:url";
import { defineConfig, devices } from "@playwright/test";

// Serves web/dist (built by web/build.sh, plus the fixtures `vite build` puts
// in web/dist/e2e) and runs the tests in Chromium. Every request off this
// server is mocked or refused in tests/mocks.ts, so no test touches the network.
const PORT = Number(process.env.E2E_PORT ?? 4317);
const CI = !!process.env.CI;

export default defineConfig({
  testDir: "tests",
  timeout: 60_000,
  expect: { timeout: 15_000 },
  fullyParallel: true,
  forbidOnly: CI,
  retries: 0,
  workers: CI ? 2 : undefined,
  reporter: CI ? [["list"], ["github"]] : "list",
  outputDir: fileURLToPath(new URL("../../target/e2e-results", import.meta.url)),
  use: {
    ...devices["Desktop Chrome"],
    baseURL: `http://127.0.0.1:${PORT}`,
    trace: "off",
    screenshot: "off",
    video: "off",
    // A browser of your own, such as one installed without `playwright install`.
    launchOptions: process.env.E2E_CHROMIUM ? { executablePath: process.env.E2E_CHROMIUM } : {},
  },
  webServer: {
    command: `python3 -m http.server --bind 127.0.0.1 --directory ${JSON.stringify(fileURLToPath(new URL("../dist", import.meta.url)))} ${PORT}`,
    url: `http://127.0.0.1:${PORT}/e2e/onedit.html`,
    reuseExistingServer: !CI,
    stdout: "ignore",
    stderr: "ignore",
  },
});
