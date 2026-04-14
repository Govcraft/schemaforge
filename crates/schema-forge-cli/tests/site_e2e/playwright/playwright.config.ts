import { defineConfig, devices } from "@playwright/test"

// Resolved from the BASE_URL env var set by run.sh, which picks an
// ephemeral Vite port for each invocation. Fallback to the default Vite
// dev port so you can also run specs against a hand-started site.
const baseURL = process.env.BASE_URL ?? "http://127.0.0.1:5173"

export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  expect: { timeout: 5_000 },
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL,
    headless: true,
    trace: "retain-on-failure",
    video: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
})
