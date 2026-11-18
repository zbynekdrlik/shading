// @ts-check
// Playwright config for the bkshading operator-panel E2E (issue 1304; the Leptos panel at / since
// shading#16's switch). The webServers, all managed + torn down by Playwright: the stdlib stub relay,
// the built bkshading service (pointed at the stub via e2e-config.toml), a service with one preview
// camera whose NDI feed is absent (e2e-preview-config.toml, issue 808), one with three cameras for
// ARRANGE and the card layouts (e2e-arrange-config.toml), and one with a self-registering handheld.
// Chromium only, one worker — keep the CI budget small. Every instance gets a fresh --layout-file in
// the temp dir, so a run never writes a layout next to the committed configs and always starts in
// number order.
const fs = require("fs");
const os = require("os");
const path = require("path");
const { defineConfig, devices } = require("@playwright/test");

const STUB_PORT = 8781;
const SVC_PORT = 8780;
// issue 808: a second service instance with ONE preview camera whose NDI feed is absent
// (e2e-preview-config.toml); panel.spec.js addresses it by absolute URL.
const PREVIEW_SVC_PORT = 8782;
// shading#16: three cameras on the stub, for ARRANGE (arrange.spec.js) and the card layouts.
const ARRANGE_SVC_PORT = 8784;
// issue 808 (owner 28.10.2026): a self-registration instance whose one camera is a handheld
// (`register:<id>`, e2e-register-next-config.toml); panel-next.spec.js announces the stub to it.
const REGISTER_NEXT_SVC_PORT = 8785;
const LAYOUT_DIR = fs.mkdtempSync(path.join(os.tmpdir(), "bkshading-e2e-layout-"));
const layout = (name) => path.join(LAYOUT_DIR, `${name}.json`);
const BIN = process.env.BKSHADING_BIN;
if (!BIN) {
  throw new Error("BKSHADING_BIN must point to the built bkshading service binary");
}
const stub = path.resolve(__dirname, "..", "stub_relay.py");
const config = path.resolve(__dirname, "e2e-config.toml");
const previewConfig = path.resolve(__dirname, "e2e-preview-config.toml");
const arrangeConfig = path.resolve(__dirname, "e2e-arrange-config.toml");
const registerNextConfig = path.resolve(__dirname, "e2e-register-next-config.toml");

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
      command: `${BIN} --config ${config} --layout-file ${layout("main")}`,
      url: `http://127.0.0.1:${SVC_PORT}/api/version`,
      reuseExistingServer: false,
      timeout: 30000,
    },
    {
      command: `${BIN} --config ${previewConfig} --layout-file ${layout("preview")}`,
      url: `http://127.0.0.1:${PREVIEW_SVC_PORT}/api/version`,
      reuseExistingServer: false,
      timeout: 30000,
    },
    {
      command: `${BIN} --config ${arrangeConfig} --layout-file ${layout("arrange")}`,
      url: `http://127.0.0.1:${ARRANGE_SVC_PORT}/api/version`,
      reuseExistingServer: false,
      timeout: 30000,
    },
    {
      command: `${BIN} --config ${registerNextConfig} --layout-file ${layout("register-next")}`,
      url: `http://127.0.0.1:${REGISTER_NEXT_SVC_PORT}/api/version`,
      reuseExistingServer: false,
      timeout: 30000,
    },
  ],
});
