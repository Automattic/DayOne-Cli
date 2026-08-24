import { defineConfig, devices } from "@playwright/test";

// Load e2e/.env (gitignored) so live, credentialed specs — e.g.
// journal-create-signatures — pick up E2E_* values without needing them
// exported in the shell. No-ops if the file is absent. Node 20.12+.
try {
  process.loadEnvFile?.(".env");
} catch {
  // .env is optional; ignore when missing.
}

const baseURL = process.env.DAYONE_WEB_BASE_URL ?? "https://stg.dayone.me";
const storageStatePath =
  process.env.PLAYWRIGHT_STORAGE_STATE ?? ".auth/staging-user.json";
const requestedWorkers = Number(process.env.PLAYWRIGHT_WORKERS);
const workers = Number.isFinite(requestedWorkers) && requestedWorkers > 0
  ? requestedWorkers
  : undefined;

export default defineConfig({
  testDir: "./tests",
  timeout: 60_000,
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers,
  reporter: process.env.CI ? [["github"], ["html", { open: "never" }]] : "list",
  outputDir: "./test-results",
  use: {
    baseURL,
    trace: "retain-on-failure",
    video: "off",
    viewport: { width: 1440, height: 1000 },
    storageState: storageStatePath,
    // Route browser traffic through a proxy when one is configured (e.g. the
    // A8C SOCKS tunnel for staging). Set PLAYWRIGHT_PROXY_SERVER, e.g.
    // `socks5://127.0.0.1:8080`.
    ...(process.env.PLAYWRIGHT_PROXY_SERVER
      ? {
          proxy: {
            server: process.env.PLAYWRIGHT_PROXY_SERVER,
            // e.g. "localhost,127.0.0.1" so a local web build loads directly
            // while its staging API calls still route through the tunnel.
            ...(process.env.PLAYWRIGHT_PROXY_BYPASS
              ? { bypass: process.env.PLAYWRIGHT_PROXY_BYPASS }
              : {})
          }
        }
      : {})
  },
  expect: {
    timeout: 15_000
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] }
    }
  ]
});
