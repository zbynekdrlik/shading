// @ts-check
// Shared helpers of the rebuilt panel's E2E (shading#16): panel-next.spec.js (parity with the JS
// panel's panel.spec.js) and arrange.spec.js (ARRANGE).
const { expect } = require("@playwright/test");

const STUB = process.env.STUB_BASE_URL || "http://127.0.0.1:8781";
/** The panel's address: the service's main web since shading#16's switch (v0.2.10). */
const PANEL = "/";

/**
 * Collects console errors/warnings and page errors; `assertClean` is every test's last assertion
 * (browser-console-zero-errors).
 */
function consoleGate(page) {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));
  return {
    problems,
    assertClean() {
      expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
    },
  };
}

/**
 * The panel's passthrough service worker would `clients.claim()` the page, after which
 * `page.route()` no longer sees its fetches (issue 1343). Neutralised before the page runs (a
 * never-settling promise, silent), never by Playwright's `serviceWorkers: "block"` (a console
 * warning).
 */
async function neutraliseServiceWorker(page) {
  await page.addInitScript(() => {
    if (navigator.serviceWorker) {
      Object.defineProperty(navigator.serviceWorker, "register", {
        value: () => new Promise(() => {}),
        configurable: true,
      });
    }
  });
}

/** Keeps the panel on its HTTP fallback poll (a deterministic offline, no reconnect churn). */
const DISABLE_WS = () => {
  window.WebSocket = function () {
    this.close = function () {};
    this.send = function () {};
    this.addEventListener = function () {};
    this.removeEventListener = function () {};
  };
};

/** Every /api/* answer 200 with a non-JSON body: the poll fails without a console error. */
async function blockApi(page, onPut) {
  await page.route("**/api/**", (route) => {
    const req = route.request();
    if (req.method() === "PUT" && onPut) {
      onPut({ url: req.url(), body: JSON.parse(req.postData() || "{}") });
      return route.fulfill({ status: 200, contentType: "application/json", body: "{}" });
    }
    return route.fulfill({ status: 200, contentType: "text/plain", body: "offline-fixture" });
  });
}

/** Waits until the WASM app has mounted and installed its push hook. */
async function waitForApp(page) {
  await page.waitForFunction(() => typeof window.shadingPush === "function", null, { timeout: 15000 });
}

/** Feeds an aggregate to the panel exactly as a /ws `state` push. */
async function pushState(page, agg) {
  await page.evaluate((a) => window.shadingPush({ type: "state", ...a }), agg);
}

/** The PUT bodies the stub relay recorded. */
async function recorded(page) {
  return (await page.request.get(`${STUB}/__recorded`)).json();
}

/** A full cam1 aggregate with overridable params (the JS panel spec's `makeFixture`). */
function makeFixture(opts = {}) {
  const g = (v, d) => (v != null ? v : d);
  return {
    version: "0.2.7-e2e",
    cameras: [
      {
        id: "cam1",
        label: "Cam 1",
        transport: "cambox-relay",
        hasPreview: !!opts.hasPreview,
        previewLive: !!opts.previewLive,
        reachable: true,
        grabFps: null,
        grabFpsDesync: false,
        fpsSync: "unknown",
        state: {
          online: true,
          camera: "Blackmagic Design Pocket Cinema Camera 4K",
          params: {
            apertureAv: g(opts.apertureAv, 4.78), // f/5.2
            apertureNorm: g(opts.apertureNorm, 2.0 / 3.0),
            iso: g(opts.iso, 400),
            kelvin: g(opts.kelvin, 5600),
            tint: g(opts.tint, 0),
            shutter: g(opts.shutter, 60),
            fps100: 6000,
            sensorFps100: 6000,
            focusDistance: null,
            irisRangePos: opts.irisRangePos != null ? opts.irisRangePos : null,
          },
          caps: {
            isoChoices: [100, 200, 400, 800],
            fNumberChoices: [2.8, 4.0, 5.2, 8.0],
            shutterChoices: [60, 100, 125],
            fpsMin: 5,
            fpsMax: 60,
            kelvinMin: 2500,
            kelvinMax: 10000,
          },
          fpsSupported: true,
          captureFps: null,
          version: "0.2.7-e2e",
          notApplied: opts.notApplied || [],
        },
      },
    ],
  };
}

module.exports = {
  STUB,
  PANEL,
  consoleGate,
  neutraliseServiceWorker,
  DISABLE_WS,
  blockApi,
  waitForApp,
  pushState,
  recorded,
  makeFixture,
};
