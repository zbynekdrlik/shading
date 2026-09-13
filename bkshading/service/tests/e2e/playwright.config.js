// @ts-check
// Playwright config for the bkshading operator-panel E2E (issue 1304). Two webServers, both
// managed + torn down by Playwright: the stdlib stub relay and the built bkshading service
// (pointed at the stub via e2e-config.toml). Chromium only, one worker — keep the CI budget small.
const path = require("path");
const { defineConfig, devices } = require("@playwright/test");

const STUB_PORT = 8781;
const SVC_PORT = 8780;
const BIN = process.env.BKSHADING_BIN;
if (!BIN) {
  throw new Error("BKSHADING_BIN must point to the built bkshading service binary");
}
const stub = path.resolve(__dirname, "..", "stub_relay.py");
const config = path.resolve(__dirname, "e2e-config.toml");

module.exports = defineConfig({
  testDir: ".",
  timeout: 45000,
  fullyParallel: false,
  workers: 1,
  reporter: "line",
  use: {
    baseURL: `http://127.0.0.1:${SVC_PORT}`,
    trace: "off",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: [
    {
      command: `python3 ${stub} --port ${STUB_PORT}`,
      url: `http://127.0.0.1:${STUB_PORT}/__recorded`,
      reuseExistingServer: false,
      timeout: 20000,
    },
    {
      command: `${BIN} --config ${config}`,
      url: `http://127.0.0.1:${SVC_PORT}/api/version`,
      reuseExistingServer: false,
      timeout: 30000,
    },
  ],
});
