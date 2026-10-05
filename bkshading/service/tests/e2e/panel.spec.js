// @ts-check
// Real-browser E2E for the issue-1304 panel +/- step buttons, extended in issue 1337 to the ISO and
// uzávierka steppers (e2e-real-user-testing.md): open the served panel, click + on clona / biely bod
// / tint / ISO / uzávierka, and assert the ABSOLUTE PUT bodies the service forwarded to the (stub)
// relay — plus a clean browser console (browser-console-zero-errors).
const fs = require("fs");
const path = require("path");
const { test, expect } = require("@playwright/test");

const STUB = process.env.STUB_BASE_URL || "http://127.0.0.1:8781";

// issue 1343: the panel registers a passthrough service worker (sw.js, issue 1305) that
// `clients.claim()`s the page on activate; from then on the page's fetches run THROUGH the SW and
// `page.route()` never sees them — the offline / not-applied tests' /api/* routes silently stopped
// intercepting after the first poll and the real service answered (CI run 35388393361). Neutralise
// the registration in EVERY test before app.js runs (a never-settling promise: app.js's `.catch`
// stays silent, nothing is logged). Not Playwright's `serviceWorkers: "block"` — that logs a
// "Service Worker registration blocked by Playwright" console WARNING, which trips the zero-console
// gate every test carries.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    if (navigator.serviceWorker) {
      Object.defineProperty(navigator.serviceWorker, "register", {
        value: () => new Promise(() => {}),
        configurable: true,
      });
    }
  });
});

test("panel +/- step buttons PUT the expected absolute values, console clean", async ({ page }) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));

  await page.goto("/");

  // The camera block renders once the service has polled the stub (online + caps). Wait for the
  // aperture "+" to be present and enabled (needs caps.fNumberChoices and a non-bound index).
  const apInc = page.locator('[data-role="aperture-inc"]');
  await expect(apInc).toBeVisible({ timeout: 15000 });
  await expect(apInc).toBeEnabled({ timeout: 15000 });

  // issue 1337: the ISO and uzávierka "+" become enabled once caps.isoChoices / caps.shutterChoices
  // arrive (stub fixture: iso 400 in [100,200,400,800]; shutter 50 below [60,100,125] = off-grid, so
  // both directions enabled — the first "+" moves it onto the grid at 60).
  const isoInc = page.locator('[data-role="iso-inc"]');
  const shInc = page.locator('[data-role="shutter-inc"]');
  await expect(isoInc).toBeEnabled({ timeout: 15000 });
  await expect(shInc).toBeEnabled({ timeout: 15000 });

  // One tap each on clona / biely bod / tint / ISO / uzávierka.
  await apInc.click();
  await page.locator('[data-role="kelvin-inc"]').click();
  await page.locator('[data-role="tint-inc"]').click();
  await isoInc.click();
  await shInc.click();

  // The service forwards each PUT to the stub relay; wait until all five land.
  await expect
    .poll(
      async () => (await (await page.request.get(`${STUB}/__recorded`)).json()).length,
      { timeout: 10000 }
    )
    .toBeGreaterThanOrEqual(5);

  const recorded = await (await page.request.get(`${STUB}/__recorded`)).json();
  // Aperture: server norm 2/3 -> choice index 2 (f/5.2) of 4; "+" steps to index 3 (f/8.0),
  // i.e. an absolute apertureNorm of 1.0.
  expect(recorded.some((b) => b.apertureNorm === 1), "aperture + -> apertureNorm 1.0").toBeTruthy();
  // Biely bod: 5600 + 100 K.
  expect(recorded.some((b) => b.kelvin === 5700), "kelvin + -> 5700").toBeTruthy();
  // Tint: 0 + 1.
  expect(recorded.some((b) => b.tint === 1), "tint + -> 1").toBeTruthy();
  // issue 1337: ISO 400 -> next enumerated choice 800 (absolute value, not a norm).
  expect(recorded.some((b) => b.iso === 800), "ISO + -> 800").toBeTruthy();
  // issue 1337: uzávierka 50 is below the first choice (60), so "+" steps onto the grid at 60.
  expect(recorded.some((b) => b.shutter === 60), "shutter + -> 60").toBeTruthy();

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

// issue 1343: disable the browser WebSocket so the panel stays on the (blockable) HTTP-poll
// fallback — a deterministic offline with no reconnect churn and no console noise. Added as an init
// script so it runs before app.js.
const DISABLE_WS = () => {
  window.WebSocket = function () {
    this.close = function () {};
    this.send = function () {};
    this.addEventListener = function () {};
    this.removeEventListener = function () {};
  };
};

test("offline banner shows when the service is unreachable and clears on reconnect, console clean", async ({
  page,
}) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));

  await page.addInitScript(DISABLE_WS);
  // Make every /api/* fetch answer 200 with a NON-JSON body so the poll's `r.json()` throw takes
  // the offline path. NOT route.abort() and NOT a 4xx/5xx fulfill: Chromium logs BOTH (a
  // `net::ERR_FAILED` / "the server responded with a status of 503") as "Failed to load resource"
  // console errors, which would trip this test's own zero-console assertion (CI run 35388393361).
  // The service worker registration is neutralised by the beforeEach above — once its
  // `clients.claim()` took the page over, page.route no longer saw these fetches.
  await page.route("**/api/**", (route) => route.fulfill({ status: 200, contentType: "text/plain", body: "offline-fixture" }));

  await page.goto("/");

  // The banner reveals ~5 s after the last contact (a fresh page's load-time grace), via the 500 ms
  // ticker, and carries a live "posledný kontakt pred N s" age counter.
  const banner = page.locator("#conn-banner");
  await expect(banner).toBeVisible({ timeout: 9000 });
  await expect(banner).toContainText(
    /Bez spojenia so službou \(posledný kontakt pred \d+ s\)/
  );

  // Unblock the poll -> the next fallback poll (<= 2 s, since the WS is disabled) succeeds and the
  // banner clears.
  await page.unroute("**/api/**");
  await expect(banner).toBeHidden({ timeout: 8000 });

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

test("a not-applied write flags the aperture value + stepper, console clean", async ({ page }) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));

  // Disable the WS and make every /api/* poll answer 200 with a non-JSON body (never route.abort()
  // or a 4xx/5xx fulfill — Chromium logs both as console errors, tripping the zero-console gate) so
  // nothing overwrites the injected fixture (the beforeEach above keeps the service worker out).
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/**", (route) => route.fulfill({ status: 200, contentType: "text/plain", body: "offline-fixture" }));

  await page.goto("/");
  await page.waitForFunction(() => typeof window.render === "function");

  // A fixture aggregate whose cam1 relay state carries notApplied:["apertureNorm"] — the camera
  // ACKed + ignored the aperture write (cam1's BMPCC today).
  const fixture = {
    version: "1.7.0-dev.e2e",
    cameras: [
      {
        id: "cam1",
        label: "Cam 1",
        transport: "cambox-relay",
        hasPreview: false,
        reachable: true,
        grabFps: null,
        grabFpsDesync: false,
        fpsSync: "unknown",
        state: {
          online: true,
          camera: "Blackmagic Design Pocket Cinema Camera 4K",
          params: {
            apertureAv: 4.78,
            apertureNorm: 2.0 / 3.0,
            iso: 400,
            kelvin: 5600,
            tint: 0,
            shutter: 50,
            fps100: 6000,
            sensorFps100: 6000,
            focusDistance: null,
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
          version: "1.7.0-dev.e2e",
          notApplied: ["apertureNorm"],
        },
      },
    ],
  };
  await page.evaluate((agg) => window.render(agg), fixture);

  // The aperture VALUE label carries the .not-applied style + the explanatory title.
  const fnum = page.locator('[data-role="fnum"]');
  await expect(fnum).toHaveClass(/not-applied/);
  await expect(fnum).toHaveAttribute("title", "Kamera tento zápis neprijala");
  // The aperture stepper is flagged too.
  await expect(page.locator('[data-role="aperture-inc"]')).toHaveClass(/not-applied/);
  // A NON-flagged value (ISO) is not styled.
  await expect(page.locator('[data-role="iso-val"]')).not.toHaveClass(/not-applied/);

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

// issue 1350: build a full aggregate fixture for cam1 with overridable params + notApplied, so the
// tests can drive render() directly and feed a controlled sequence of server pushes (stale /
// confirming / refused) without racing the real ~2 s pump. Mirrors the not-applied test's fixture.
function makeFixture(opts = {}) {
  const g = (v, d) => (v != null ? v : d);
  return {
    version: "1.7.0-dev.e2e",
    cameras: [
      {
        id: "cam1",
        label: "Cam 1",
        transport: "cambox-relay",
        // issue 808: a preview-capable camera + whether its feed is live (both default off).
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
            apertureNorm: 2.0 / 3.0,
            iso: g(opts.iso, 400),
            kelvin: g(opts.kelvin, 5600),
            tint: g(opts.tint, 0),
            shutter: g(opts.shutter, 60),
            fps100: 6000,
            sensorFps100: 6000,
            focusDistance: null,
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
          version: "1.7.0-dev.e2e",
          notApplied: opts.notApplied || [],
        },
      },
    ],
  };
}

test("an optimistic tap holds its target against a stale pump snapshot, clears on confirmation, console clean", async ({
  page,
}) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));

  // Disable the WS + answer /api/* 200 non-JSON so ONLY our window.render pushes drive the panel
  // (never route.abort()/4xx — Chromium logs both as console errors; the beforeEach neutralises SW).
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/**", (route) =>
    route.fulfill({ status: 200, contentType: "text/plain", body: "offline-fixture" })
  );
  await page.goto("/");
  await page.waitForFunction(() => typeof window.render === "function");

  // Establish server truth: clona f/5.2, biely bod 5600 K.
  await page.evaluate((agg) => window.render(agg), makeFixture({}));
  const fnum = page.locator('[data-role="fnum"]');
  const kval = page.locator('[data-role="kelvin-val"]');
  await expect(fnum).toHaveText("f/5.2");
  await expect(kval).toHaveText("5600K");

  // Tap clona + (f/5.2 -> f/8.0) and biely bod + (5600 -> 5700). The optimistic target shows pending.
  await page.locator('[data-role="aperture-inc"]').click();
  await page.locator('[data-role="kelvin-inc"]').click();
  await expect(fnum).toHaveText("f/8.0");
  await expect(fnum).toHaveClass(/pending/);
  await expect(kval).toHaveText("5700K");
  await expect(kval).toHaveClass(/pending/);

  // A STALE pump snapshot (captured BEFORE the camera applied the write) carries the OLD values. The
  // panel must NOT flip the number down — it holds the target with .pending (issue 1350; the bug this
  // fixes is the down-then-back flicker, so this render is what was RED before the fix).
  await page.evaluate((agg) => window.render(agg), makeFixture({ apertureAv: 4.78, kelvin: 5600 }));
  await expect(fnum).toHaveText("f/8.0");
  await expect(fnum).toHaveClass(/pending/);
  await expect(kval).toHaveText("5700K");
  await expect(kval).toHaveClass(/pending/);

  // The confirming push (the camera applied the write; f/8.0 <- apertureAv 6.0, 5700 K) matches the
  // held target -> .pending clears and the number is solid.
  await page.evaluate((agg) => window.render(agg), makeFixture({ apertureAv: 6.0, kelvin: 5700 }));
  await expect(fnum).toHaveText("f/8.0");
  await expect(fnum).not.toHaveClass(/pending/);
  await expect(kval).toHaveText("5700K");
  await expect(kval).not.toHaveClass(/pending/);

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

test("a not-applied push after an optimistic tap surfaces the refusal (clears the hold), console clean", async ({
  page,
}) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));

  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/**", (route) =>
    route.fulfill({ status: 200, contentType: "text/plain", body: "offline-fixture" })
  );
  await page.goto("/");
  await page.waitForFunction(() => typeof window.render === "function");

  await page.evaluate((agg) => window.render(agg), makeFixture({}));
  const fnum = page.locator('[data-role="fnum"]');
  await expect(fnum).toHaveText("f/5.2");

  // Tap clona + -> optimistic pending target f/8.0.
  await page.locator('[data-role="aperture-inc"]').click();
  await expect(fnum).toHaveText("f/8.0");
  await expect(fnum).toHaveClass(/pending/);

  // The relay reports the camera REFUSED the aperture write (issue 1343 notApplied) while the pump
  // snapshot still carries the OLD value. The pending hold must NOT swallow the refusal: it clears,
  // the reverted server value shows, and the .not-applied style + title surface immediately.
  await page.evaluate(
    (agg) => window.render(agg),
    makeFixture({ apertureAv: 4.78, notApplied: ["apertureNorm"] })
  );
  await expect(fnum).toHaveText("f/5.2");
  await expect(fnum).not.toHaveClass(/pending/);
  await expect(fnum).toHaveClass(/not-applied/);
  await expect(fnum).toHaveAttribute("title", "Kamera tento zápis neprijala");

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

// --- issue 808: the preview of a camera whose NDI feed is absent / stopped ---------------------
// The second service instance (playwright.config.js, e2e-preview-config.toml) has ONE camera WITH
// an `ndi_preview` and no feed (CI has no libndi, the source does not exist). Before the fix the
// endpoint answered 503 and the panel reloaded the <img> at 3 Hz: a console error every ~1.6 s, and
// a feed that stopped after delivering frames kept its last frame on screen as if live.
const PREVIEW_SVC = process.env.PREVIEW_SVC_BASE_URL || "http://127.0.0.1:8782";
const PREVIEW_URL_RE = /\/api\/cameras\/[^/]+\/preview\.jpg/;
// A valid 16x9 JPEG (PIL, quality 60) for the fixture-driven loader test.
const PREVIEW_JPEG = fs.readFileSync(path.join(__dirname, "fixtures", "preview-16x9.jpg"));

test("a preview camera with no NDI feed shows the placeholder, never a 4xx/5xx, console clean (issue 808)", async ({
  page,
}) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));
  const previewRequests = [];
  const badPreview = [];
  page.on("request", (req) => {
    if (PREVIEW_URL_RE.test(req.url())) previewRequests.push(req.url());
  });
  page.on("response", (res) => {
    if (PREVIEW_URL_RE.test(res.url()) && res.status() >= 400) {
      badPreview.push(`${res.status()} ${res.url()}`);
    }
  });

  await page.goto(`${PREVIEW_SVC}/`);

  // The service reports the camera preview-capable, but its feed NOT live.
  const agg = await (await page.request.get(`${PREVIEW_SVC}/api/cameras`)).json();
  const cam = agg.cameras.find((c) => c.id === "cam1");
  expect(cam.hasPreview, "cam1 is configured with an ndi_preview").toBe(true);
  expect(cam.previewLive, "no frame -> previewLive false").toBe(false);

  // The preview block is there, showing its placeholder, no image.
  const preview = page.locator('[data-role="preview"]');
  const placeholder = page.locator('[data-role="preview-placeholder"]');
  const img = page.locator('[data-role="preview-img"]');
  await expect(preview).toBeVisible({ timeout: 15000 });
  await expect(placeholder).toBeVisible();
  await expect(placeholder).toHaveText("NDI preview — čakám…");

  // Several 3 Hz refresh periods plus a 2 s pump tick: still the placeholder, nothing logged.
  await page.waitForTimeout(3500);
  await expect(placeholder).toBeVisible();
  await expect(img).not.toHaveClass(/ready/);

  // The endpoint itself: a configured camera with no fresh frame is 204 + no-store; only an
  // unknown camera id is a 404 (read via the API context, which never logs to the page console).
  const r = await page.request.get(`${PREVIEW_SVC}/api/cameras/cam1/preview.jpg`);
  expect(r.status(), "no fresh frame -> 204").toBe(204);
  expect(r.headers()["cache-control"] || "").toContain("no-store");
  const unknown = await page.request.get(`${PREVIEW_SVC}/api/cameras/no-such-cam/preview.jpg`);
  expect(unknown.status(), "unknown camera -> 404").toBe(404);

  expect(previewRequests, "a not-live preview is never fetched by the panel").toEqual([]);
  expect(badPreview, `4xx/5xx preview responses: ${badPreview.join(" | ")}`).toEqual([]);
  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

test("a hung preview fetch is aborted after its timeout, frees the block for the next one, and recovers, console clean (issue 808)", async ({
  page,
}) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));

  // The feed is live, but the first preview request hangs for 8 s (a half-open link). The panel's
  // 2 s fetch timeout must abort it (an aborted fetch logs nothing) and free the block's single
  // in-flight slot, so a second request arrives (~2.7 s) long before the first one would answer.
  let hang = true;
  let previewHits = 0;
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/cameras", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(makeFixture({ hasPreview: true, previewLive: true })),
    })
  );
  await page.route(PREVIEW_URL_RE, async (route) => {
    previewHits += 1;
    if (hang) {
      await new Promise((resolve) => setTimeout(resolve, 8000));
      try {
        await route.fulfill({ status: 200, contentType: "image/jpeg", body: PREVIEW_JPEG });
      } catch (e) {
        // the page aborted this request already -- exactly what the test wants
      }
      return;
    }
    return route.fulfill({ status: 200, contentType: "image/jpeg", body: PREVIEW_JPEG });
  });

  await page.goto(`${PREVIEW_SVC}/`);
  const img = page.locator('[data-role="preview-img"]');
  const placeholder = page.locator('[data-role="preview-placeholder"]');

  // A second request while the first is still held (8 s) = the timeout freed the slot.
  await expect
    .poll(() => previewHits, { timeout: 6000, intervals: [100] })
    .toBeGreaterThanOrEqual(2);
  await expect(placeholder).toBeVisible();
  await expect(placeholder).toHaveText("NDI preview — čakám…");
  await expect(img).not.toHaveClass(/ready/);

  // The link recovers: the next request answers and the frame shows.
  hang = false;
  await expect(img).toHaveClass(/ready/, { timeout: 8000 });
  await expect(placeholder).toBeHidden();

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

test("a timed-out fetch keeps a frame younger than 3 s, then the stall drops it to the stopped placeholder, console clean (issue 808)", async ({
  page,
}) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));

  // Frames arrive until the link stalls: from then on every preview request is held 8 s. The
  // first held request times out (2 s) while the last frame is ~2.3 s old -- that frame must stay
  // (no flash of the stopped placeholder). The next held request times out when the frame is
  // over 3 s old -- then the stopped placeholder must show (a bounded freeze, never a frozen frame).
  let stall = false;
  let held = 0;
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/cameras", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(makeFixture({ hasPreview: true, previewLive: true })),
    })
  );
  await page.route(PREVIEW_URL_RE, async (route) => {
    if (stall) {
      held += 1;
      await new Promise((resolve) => setTimeout(resolve, 8000));
      try {
        await route.fulfill({ status: 200, contentType: "image/jpeg", body: PREVIEW_JPEG });
      } catch (e) {
        // the page aborted this request already
      }
      return;
    }
    return route.fulfill({ status: 200, contentType: "image/jpeg", body: PREVIEW_JPEG });
  });

  await page.goto(`${PREVIEW_SVC}/`);
  const img = page.locator('[data-role="preview-img"]');
  const placeholder = page.locator('[data-role="preview-placeholder"]');
  await expect(img).toHaveClass(/ready/, { timeout: 10000 });

  stall = true;
  // A second held request means the first one already timed out: the young frame is still shown.
  await expect.poll(() => held, { timeout: 6000, intervals: [100] }).toBeGreaterThanOrEqual(2);
  expect(
    await img.evaluate((e) => e.classList.contains("ready")),
    "a frame younger than 3 s survives one timed-out fetch"
  ).toBe(true);
  // The second held request times out with the frame over 3 s old: the stopped placeholder.
  await expect(placeholder).toBeVisible({ timeout: 6000 });
  await expect(placeholder).toHaveText("NDI preview — obraz sa zastavil, čakám…");
  await expect(img).not.toHaveClass(/ready/);

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

test("a live preview frame is shown, a stopped feed drops to the placeholder and is no longer fetched, console clean (issue 808)", async ({
  page,
}) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));

  // The HTTP-fallback poll (WS disabled) serves the fixture aggregate; the preview endpoint serves a
  // fresh JPEG until the feed "stops", then 204 — exactly what the service does for a stale frame.
  let live = true;
  let frame = true;
  let previewHits = 0;
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/cameras", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(makeFixture({ hasPreview: true, previewLive: live })),
    })
  );
  await page.route(PREVIEW_URL_RE, (route) => {
    previewHits += 1;
    if (frame) {
      return route.fulfill({
        status: 200,
        contentType: "image/jpeg",
        headers: { "cache-control": "no-store" },
        body: PREVIEW_JPEG,
      });
    }
    return route.fulfill({ status: 204, headers: { "cache-control": "no-store" }, body: "" });
  });

  await page.goto(`${PREVIEW_SVC}/`);
  const img = page.locator('[data-role="preview-img"]');
  const placeholder = page.locator('[data-role="preview-placeholder"]');

  // A live feed: the frame shows, loaded as an object URL (fetch -> blob), the placeholder hides.
  await expect(img).toHaveClass(/ready/, { timeout: 10000 });
  await expect(placeholder).toBeHidden();
  expect(await img.getAttribute("src")).toMatch(/^blob:/);

  // The feed stops: the endpoint answers 204 before the next pump flips previewLive. The frozen
  // frame must go, replaced by the "stopped" placeholder.
  frame = false;
  await expect(placeholder).toBeVisible({ timeout: 5000 });
  await expect(placeholder).toHaveText("NDI preview — obraz sa zastavil, čakám…");
  await expect(img).not.toHaveClass(/ready/);

  // The pump reports the feed not live: the panel stops fetching it. Wait for the block to take
  // that push (the next 2 s fallback poll) and for any fetch already in flight to finish, then no
  // further preview request may arrive over ~4 more refresh periods.
  live = false;
  const block = page.locator('[data-role="camera-block"]');
  await expect
    .poll(() => block.evaluate((e) => `${e.dataset.previewLive}/${e.dataset.previewBusy}`), {
      timeout: 8000,
    })
    .toBe("0/0");
  const hitsWhenNotLive = previewHits;
  await page.waitForTimeout(1500);
  expect(previewHits, "a not-live preview is not fetched").toBe(hitsWhenNotLive);
  await expect(placeholder).toBeVisible();

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

// Owner 4.10.2026: "v shadingu maju byt online camery na zaciatku podla cisiel, a potom ofline".
// Cameras with a live camera come first in config (= number) order, then every other block, and the
// order follows a camera going online/offline on a later push.
test("online cameras come first in number order, then the offline ones, console clean", async ({ page }) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/**", (route) => route.fulfill({ status: 200, contentType: "text/plain", body: "offline-fixture" }));
  await page.goto("/");
  await page.waitForFunction(() => typeof window.render === "function");

  const cam = (id, reachable, online) => ({
    id,
    label: id,
    transport: id.startsWith("handheld") ? "sbc-relay" : "cambox-relay",
    hasPreview: false,
    reachable,
    grabFps: null,
    grabFpsDesync: false,
    fpsSync: "unknown",
    state: reachable
      ? {
          online,
          camera: online ? "USB PTP Class Camera" : null,
          params: { apertureAv: null, apertureNorm: null, iso: null, kelvin: null, tint: null,
            shutter: null, fps100: null, sensorFps100: null, focusDistance: null },
          caps: null,
          fpsSupported: false,
          captureFps: null,
          version: "1.7.0-dev.e2e",
          notApplied: [],
        }
      : null,
  });
  const order = () => page.locator("#camera-grid > [data-id]").evaluateAll((els) => els.map((e) => e.dataset.id));

  // cam1 + cam2 + handheld-1 live, cam3 relay up without a camera, cam4 relay down
  await page.evaluate((agg) => window.render(agg), {
    version: "1.7.0-dev.e2e",
    cameras: [cam("cam1", true, true), cam("cam2", true, true), cam("cam3", true, false),
      cam("cam4", false, false), cam("handheld-1", true, true)],
  });
  expect(await order()).toEqual(["cam1", "cam2", "handheld-1", "cam3", "cam4"]);

  // cam1 drops its camera, cam4 comes online: the blocks follow on the next push
  await page.evaluate((agg) => window.render(agg), {
    version: "1.7.0-dev.e2e",
    cameras: [cam("cam1", true, false), cam("cam2", true, true), cam("cam3", true, false),
      cam("cam4", true, true), cam("handheld-1", true, true)],
  });
  expect(await order()).toEqual(["cam2", "cam4", "handheld-1", "cam1", "cam3"]);

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});

// issue 1402: on every BMPCC, d006 is the PROJECT fps (a MENU of the camera's own timebases, x100)
// and d007 the OFF-SPEED fps. The "Zosúladiť s grab" button is offered only when the camera would
// take the write: the service's `fpsAlignSettable` (grab x100 is one of the camera's d006 choices).
// cam3 (the Pocket 6K) at 60.00 against a 60 grab is Synced -> no button; a camera at 50.00
// against the 60 grab is a real mismatch, but 6000 is not a d006 choice -> the warning, no button;
// a camera at 25.00 against a 50 grab (5000 IS listed) gets the button, and its click sends
// `{fps: 50}`. A camera whose view lacks `fpsAlignSettable` (an older service) never gets it, even
// with `fpsSupported`.
test("the align-to-grab button is offered only for a grab the camera lists, console clean (issue 1402)", async ({
  page,
}) => {
  const problems = [];
  page.on("console", (msg) => {
    const t = msg.type();
    if (t === "error" || t === "warning") problems.push(`${t}: ${msg.text()}`);
  });
  page.on("pageerror", (err) => problems.push(`pageerror: ${err.message}`));
  await page.addInitScript(DISABLE_WS);
  const puts = [];
  await page.route("**/api/**", (route) => {
    const req = route.request();
    if (req.method() === "PUT") puts.push({ url: req.url(), body: req.postDataJSON() });
    return route.fulfill({ status: 200, contentType: "text/plain", body: "offline-fixture" });
  });
  await page.goto("/");
  await page.waitForFunction(() => typeof window.render === "function");

  const BMPCC_D006 = [8, 2398, 2400, 2500, 2997, 3000, 5000, 5994];
  const view = ({ fps100, sensorFps100, grabFps, fpsSync, fpsAlignSettable }) => {
    const v = {
      id: "cam3",
      label: "Cam 3",
      transport: "sbc-relay",
      hasPreview: false,
      reachable: true,
      grabFps,
      grabFpsDesync: false,
      fpsSync,
      state: {
        online: true,
        camera: "Blackmagic Design Pocket Cinema Camera 6K",
        params: { apertureAv: 4.78, apertureNorm: 2.0 / 3.0, iso: 400, kelvin: 5600, tint: 0,
          shutter: 120, fps100, sensorFps100, focusDistance: null },
        caps: { isoChoices: [100, 200, 400, 800], fNumberChoices: [2.8, 4.0, 5.2, 8.0],
          shutterChoices: [60, 100, 120, 125], fpsChoices: BMPCC_D006, fpsMin: 5, fpsMax: 60,
          kelvinMin: 2500, kelvinMax: 10000 },
        fpsSupported: true,
        captureFps: null,
        version: "1.7.0-dev.e2e",
        notApplied: [],
      },
    };
    if (fpsAlignSettable !== undefined) v.fpsAlignSettable = fpsAlignSettable;
    return { version: "1.7.0-dev.e2e", cameras: [v] };
  };
  const show = (opts) => page.evaluate((agg) => window.render(agg), view(opts));
  const fpsVal = page.locator('[data-role="fps-val"]');
  const warn = page.locator('[data-role="fps-warn"]');
  const btn = page.locator('[data-role="fps-set-grab"]');

  // cam3 read right: project 60.00 (d006), off-speed 50 (d007), grab 60 -> synced, no button.
  await show({ fps100: 6000, sensorFps100: 5000, grabFps: 60, fpsSync: "synced", fpsAlignSettable: false });
  await expect(fpsVal).toHaveText("60.00");
  await expect(warn).toBeHidden();
  await expect(btn).toBeHidden();

  // A real mismatch the camera cannot be aligned to (6000 is not a d006 choice): warning, no button.
  await show({ fps100: 5000, sensorFps100: 5000, grabFps: 60, fpsSync: "mismatch", fpsAlignSettable: false });
  await expect(warn).toBeVisible();
  await expect(warn).toHaveText("⚠ kamera 50.00 ≠ grab 60");
  await expect(btn).toBeHidden();

  // An older service view without fpsAlignSettable: never offered, even with fpsSupported.
  await show({ fps100: 5000, sensorFps100: 5000, grabFps: 60, fpsSync: "mismatch" });
  await expect(warn).toBeVisible();
  await expect(btn).toBeHidden();

  // A grab the camera lists (50 -> 5000): the button is offered and its click sends {fps: 50}.
  await show({ fps100: 2500, sensorFps100: 2500, grabFps: 50, fpsSync: "mismatch", fpsAlignSettable: true });
  await expect(btn).toBeVisible();
  await expect(btn).toHaveText("Zosúladiť s grab (50)");
  await btn.click();
  await expect.poll(() => puts.length).toBe(1);
  expect(puts[0].url).toContain("/api/cameras/cam3/params");
  expect(puts[0].body).toEqual({ fps: 50 });

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});
