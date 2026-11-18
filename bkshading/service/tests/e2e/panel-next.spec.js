// @ts-check
// Real-browser E2E of the REBUILT panel at /next/ (shading#16, Leptos CSR): every scenario of the
// JS panel's panel.spec.js, ported one-to-one to the new panel (parity), each ending on a clean
// console. Fixtures go in through `window.shadingPush`, the panel's own entry for a /ws message.
// The old panel at / keeps its own panel.spec.js until the switch.
const fs = require("fs");
const path = require("path");
const { test, expect } = require("@playwright/test");
const {
  NEXT,
  consoleGate,
  neutraliseServiceWorker,
  DISABLE_WS,
  blockApi,
  waitForApp,
  pushState,
  recorded,
  makeFixture,
} = require("./next-helpers");

test.beforeEach(async ({ page }) => {
  await neutraliseServiceWorker(page);
});

test("the rebuilt panel shows the service's version, console clean (version-on-dashboard)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.goto(NEXT);
  const svc = await (await page.request.get("/api/version")).json();
  await expect(page.locator('[data-role="version"]')).toHaveText(`v${svc.version}`, { timeout: 15000 });
  await expect(page.locator('[data-role="conn"]')).toHaveText("spojené");
  gate.assertClean();
});

test("+/- steppers PUT the expected absolute values, console clean (issue 1304/1337)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.goto(NEXT);

  const apInc = page.locator('[data-role="aperture-inc"]');
  await expect(apInc).toBeEnabled({ timeout: 15000 });
  const isoInc = page.locator('[data-role="iso-inc"]');
  const shInc = page.locator('[data-role="shutter-inc"]');
  await expect(isoInc).toBeEnabled();
  await expect(shInc).toBeEnabled();

  const before = (await recorded(page)).length;
  await apInc.click();
  await page.locator('[data-role="kelvin-inc"]').click();
  await page.locator('[data-role="tint-inc"]').click();
  await isoInc.click();
  await shInc.click();

  await expect.poll(async () => (await recorded(page)).length, { timeout: 10000 }).toBeGreaterThanOrEqual(before + 5);
  const got = (await recorded(page)).slice(before);
  expect(got.some((b) => b.apertureStep === 1), "aperture + -> apertureStep 1").toBeTruthy();
  expect(got.some((b) => b.apertureNorm != null), "+/- never send a grid position").toBeFalsy();
  expect(got.some((b) => b.kelvin === 5700), "kelvin + -> 5700").toBeTruthy();
  expect(got.some((b) => b.tint === 1), "tint + -> 1").toBeTruthy();
  expect(got.some((b) => b.iso === 800), "ISO + -> 800").toBeTruthy();
  expect(got.some((b) => b.shutter === 60), "shutter 50 (off-grid) + -> 60").toBeTruthy();
  // The stub relay sends no fps choices: the fps stepper is disabled, never a guessed rate.
  await expect(page.locator('[data-role="fps-inc"]')).toBeDisabled();
  gate.assertClean();
});

test("the fps stepper writes only a whole rate the camera lists, console clean (shading#16, issue 1402)", async ({ page }) => {
  const gate = consoleGate(page);
  const puts = [];
  await page.addInitScript(DISABLE_WS);
  await blockApi(page, (p) => puts.push(p));
  await page.goto(NEXT);
  await waitForApp(page);
  const agg = makeFixture({});
  agg.cameras[0].state.caps.fpsChoices = [8, 2398, 2400, 2500, 2997, 3000, 5000, 5994, 6000];
  await pushState(page, agg);
  const val = page.locator('[data-role="fps-val"]');
  await expect(val).toHaveText("60.00");
  await expect(page.locator('[data-role="fps-inc"]')).toBeDisabled();
  await page.locator('[data-role="fps-dec"]').click();
  await expect(val).toHaveText("50.00");
  await expect(val).toHaveClass(/pending/);
  await expect.poll(() => puts.length).toBe(1);
  expect(puts[0].url).toContain("/api/cameras/cam1/params");
  expect(puts[0].body).toEqual({ fps: 50 });
  gate.assertClean();
});

test("offline banner shows when the service is unreachable and clears on reconnect, console clean (issue 1343)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.addInitScript(DISABLE_WS);
  await blockApi(page);
  await page.goto(NEXT);

  const banner = page.locator("#conn-banner");
  await expect(banner).toBeVisible({ timeout: 9000 });
  await expect(banner).toContainText(/Bez spojenia so službou · posledný kontakt pred \d+ s/);
  await expect(page.locator('[data-role="conn"]')).toHaveText("odpojené");

  await page.unroute("**/api/**");
  await expect(banner).toBeHidden({ timeout: 8000 });
  await expect(page.locator('[data-role="camera-block"]')).toHaveCount(1);
  gate.assertClean();
});

test("a not-applied write flags the aperture value + stepper, console clean (issue 1343)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.addInitScript(DISABLE_WS);
  await blockApi(page);
  await page.goto(NEXT);
  await waitForApp(page);
  await pushState(page, makeFixture({ notApplied: ["apertureNorm"] }));

  const fnum = page.locator('[data-role="fnum"]');
  await expect(fnum).toHaveClass(/not-applied/);
  await expect(fnum).toHaveAttribute("title", "Kamera tento zápis neprijala");
  await expect(page.locator('[data-role="aperture-inc"]')).toHaveClass(/not-applied/);
  await expect(page.locator('[data-role="iso-val"]')).not.toHaveClass(/not-applied/);
  gate.assertClean();
});

test("an optimistic tap holds its target against a stale pump snapshot, clears on confirmation, console clean (issue 1350)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.addInitScript(DISABLE_WS);
  await blockApi(page, () => {});
  await page.goto(NEXT);
  await waitForApp(page);

  await pushState(page, makeFixture({}));
  const fnum = page.locator('[data-role="fnum"]');
  const kval = page.locator('[data-role="kelvin-val"]');
  await expect(fnum).toHaveText("f/5.2");
  await expect(kval).toHaveText("5600 K");

  await page.locator('[data-role="aperture-inc"]').click();
  await page.locator('[data-role="kelvin-inc"]').click();
  await expect(fnum).toHaveText("f/5.2");
  await expect(fnum).toHaveClass(/pending/);
  await expect(kval).toHaveText("5700 K");
  await expect(kval).toHaveClass(/pending/);

  // A stale snapshot (taken before the camera applied the write) must not flip the numbers back.
  await pushState(page, makeFixture({ apertureAv: 4.78, kelvin: 5600 }));
  await expect(fnum).toHaveText("f/5.2");
  await expect(fnum).toHaveClass(/pending/);
  await expect(kval).toHaveText("5700 K");
  await expect(kval).toHaveClass(/pending/);

  // The confirming push: the lens moved to f/8.0, the white balance is 5700 K.
  await pushState(page, makeFixture({ apertureAv: 6.0, kelvin: 5700 }));
  await expect(fnum).toHaveText("f/8.0");
  await expect(fnum).not.toHaveClass(/pending/);
  await expect(kval).toHaveText("5700 K");
  await expect(kval).not.toHaveClass(/pending/);
  gate.assertClean();
});

test("a not-applied push after an optimistic tap surfaces the refusal (clears the hold), console clean (issue 1350)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.addInitScript(DISABLE_WS);
  await blockApi(page, () => {});
  await page.goto(NEXT);
  await waitForApp(page);

  await pushState(page, makeFixture({}));
  const fnum = page.locator('[data-role="fnum"]');
  await expect(fnum).toHaveText("f/5.2");
  await page.locator('[data-role="aperture-inc"]').click();
  await expect(fnum).toHaveClass(/pending/);

  await pushState(page, makeFixture({ apertureAv: 4.78, notApplied: ["apertureStep"] }));
  await expect(fnum).toHaveText("f/5.2");
  await expect(fnum).not.toHaveClass(/pending/);
  await expect(fnum).toHaveClass(/not-applied/);
  await expect(fnum).toHaveAttribute("title", "Kamera tento zápis neprijala");
  gate.assertClean();
});

// shading#6 (0.2.3): the panel knows the lens's ends from `irisRangePos` (0 wide open, 1 closed),
// also where the f-number grid reaches further (6K fw 8.6: grid from f/2.8, lens from f/4.0).
test("the aperture stepper stops at the lens's ends (irisRangePos), console clean (shading#6)", async ({ page }) => {
  const gate = consoleGate(page);
  const puts = [];
  await page.addInitScript(DISABLE_WS);
  await blockApi(page, (p) => puts.push(p));
  await page.goto(NEXT);
  await waitForApp(page);

  const dec = page.locator('[data-role="aperture-dec"]');
  const inc = page.locator('[data-role="aperture-inc"]');
  const fnum = page.locator('[data-role="fnum"]');
  const end = page.locator('.row[data-row="aperture"] .end');

  await pushState(page, makeFixture({ apertureAv: 4.0, apertureNorm: 1 / 3, irisRangePos: 0.0 }));
  await expect(fnum).toHaveText("f/4.0");
  await expect(dec).toBeDisabled();
  await expect(dec).toHaveAttribute("title", "Objektív je na najotvorenejšej clone");
  await expect(inc).toBeEnabled();
  await expect(end).toBeVisible();

  await pushState(page, makeFixture({ apertureAv: 4.78, irisRangePos: 0.4 }));
  await expect(dec).toBeEnabled();
  await expect(inc).toBeEnabled();
  await expect(end).toBeHidden();

  await pushState(page, makeFixture({ apertureAv: 4.78, irisRangePos: 1.0 }));
  await expect(inc).toBeDisabled();
  await expect(dec).toBeEnabled();
  await dec.click();
  await expect.poll(() => puts.length).toBe(1);
  expect(puts[0].body).toEqual({ apertureStep: -1 });
  gate.assertClean();
});

// --- issue 808: the preview of a camera whose NDI feed is absent / stopped ---------------------
const PREVIEW_SVC = process.env.PREVIEW_SVC_BASE_URL || "http://127.0.0.1:8782";
const PREVIEW_URL_RE = /\/api\/cameras\/[^/]+\/preview\.jpg/;
const PREVIEW_JPEG = fs.readFileSync(path.join(__dirname, "fixtures", "preview-16x9.jpg"));

test("a preview camera with no NDI feed shows the placeholder, never a 4xx/5xx, console clean (issue 808)", async ({ page }) => {
  const gate = consoleGate(page);
  const previewRequests = [];
  page.on("request", (req) => {
    if (PREVIEW_URL_RE.test(req.url())) previewRequests.push(req.url());
  });
  await page.goto(`${PREVIEW_SVC}${NEXT}`);

  const preview = page.locator('[data-role="preview"]');
  const placeholder = page.locator('[data-role="preview-placeholder"]');
  await expect(preview).toBeVisible({ timeout: 15000 });
  await expect(placeholder).toBeVisible();
  await expect(placeholder).toHaveText("NDI preview — čakám…");
  await page.waitForTimeout(3500);
  await expect(placeholder).toBeVisible();
  await expect(page.locator('[data-role="preview-img"]')).not.toHaveClass(/ready/);
  expect(previewRequests, "a not-live preview is never fetched by the panel").toEqual([]);
  gate.assertClean();
});

test("a hung preview fetch is aborted after its timeout, frees the block, and recovers, console clean (issue 808)", async ({ page }) => {
  const gate = consoleGate(page);
  let hang = true;
  let previewHits = 0;
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/cameras", (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(makeFixture({ hasPreview: true, previewLive: true })) })
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

  await page.goto(`${PREVIEW_SVC}${NEXT}`);
  const img = page.locator('[data-role="preview-img"]');
  const placeholder = page.locator('[data-role="preview-placeholder"]');
  await expect.poll(() => previewHits, { timeout: 8000, intervals: [100] }).toBeGreaterThanOrEqual(2);
  await expect(placeholder).toHaveText("NDI preview — čakám…");
  await expect(img).not.toHaveClass(/ready/);

  hang = false;
  await expect(img).toHaveClass(/ready/, { timeout: 8000 });
  await expect(placeholder).toBeHidden();
  gate.assertClean();
});

test("a timed-out fetch keeps a frame younger than 3 s, then the stall drops it, console clean (issue 808)", async ({ page }) => {
  const gate = consoleGate(page);
  let stall = false;
  let held = 0;
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/cameras", (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(makeFixture({ hasPreview: true, previewLive: true })) })
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

  await page.goto(`${PREVIEW_SVC}${NEXT}`);
  const img = page.locator('[data-role="preview-img"]');
  const placeholder = page.locator('[data-role="preview-placeholder"]');
  await expect(img).toHaveClass(/ready/, { timeout: 12000 });

  stall = true;
  await expect.poll(() => held, { timeout: 6000, intervals: [100] }).toBeGreaterThanOrEqual(2);
  expect(await img.evaluate((e) => e.classList.contains("ready")), "a frame younger than 3 s survives one timed-out fetch").toBe(true);
  await expect(placeholder).toBeVisible({ timeout: 6000 });
  await expect(placeholder).toHaveText("NDI preview — obraz sa zastavil, čakám…");
  await expect(img).not.toHaveClass(/ready/);
  gate.assertClean();
});

test("a live preview frame is shown, a stopped feed drops to the placeholder and is no longer fetched, console clean (issue 808)", async ({ page }) => {
  const gate = consoleGate(page);
  let live = true;
  let frame = true;
  let previewHits = 0;
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/cameras", (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(makeFixture({ hasPreview: true, previewLive: live })) })
  );
  await page.route(PREVIEW_URL_RE, (route) => {
    previewHits += 1;
    if (frame) {
      return route.fulfill({ status: 200, contentType: "image/jpeg", headers: { "cache-control": "no-store" }, body: PREVIEW_JPEG });
    }
    return route.fulfill({ status: 204, headers: { "cache-control": "no-store" }, body: "" });
  });

  await page.goto(`${PREVIEW_SVC}${NEXT}`);
  const img = page.locator('[data-role="preview-img"]');
  const placeholder = page.locator('[data-role="preview-placeholder"]');
  await expect(img).toHaveClass(/ready/, { timeout: 12000 });
  await expect(placeholder).toBeHidden();
  expect(await img.getAttribute("src")).toMatch(/^blob:/);

  frame = false;
  await expect(placeholder).toBeVisible({ timeout: 5000 });
  await expect(placeholder).toHaveText("NDI preview — obraz sa zastavil, čakám…");
  await expect(img).not.toHaveClass(/ready/);
  expect(await img.getAttribute("src"), "no frozen frame: the source is removed").toBeNull();

  live = false;
  const block = page.locator('[data-role="camera-block"]');
  await expect
    .poll(() => block.evaluate((e) => `${e.dataset.previewLive}/${e.dataset.previewBusy}`), { timeout: 8000 })
    .toBe("0/0");
  const hitsWhenNotLive = previewHits;
  await page.waitForTimeout(1500);
  expect(previewHits, "a not-live preview is not fetched").toBe(hitsWhenNotLive);
  await expect(placeholder).toBeVisible();
  gate.assertClean();
});

test("a block removed while its preview fetch is in flight leaves a clean console (shading#16 review)", async ({ page }) => {
  const gate = consoleGate(page);
  let held = 0;
  let hidden = false;
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/cameras", (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(makeFixture({ hasPreview: true, previewLive: true })) })
  );
  await page.route("**/api/layout", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(hidden ? { rev: 1, order: ["cam1"], hidden: ["cam1"] } : { rev: 0, order: [], hidden: [] }),
    })
  );
  await page.route(PREVIEW_URL_RE, async (route) => {
    held += 1;
    await new Promise((resolve) => setTimeout(resolve, 1500));
    try {
      await route.fulfill({ status: 200, contentType: "image/jpeg", body: PREVIEW_JPEG });
    } catch (e) {
      // aborted by the page
    }
  });
  await page.goto(`${PREVIEW_SVC}${NEXT}`);
  await waitForApp(page);
  await expect.poll(() => held, { timeout: 8000, intervals: [100] }).toBeGreaterThanOrEqual(1);
  // The venue hides the camera (another device's save) while its frame is still on its way.
  hidden = true;
  await page.evaluate(() => window.shadingPush({ type: "layout", rev: 1, order: ["cam1"], hidden: ["cam1"] }));
  await expect(page.locator('[data-role="camera-block"]')).toHaveCount(0);
  // The held fetch answers or is aborted after the block is gone.
  await page.waitForTimeout(2500);
  await expect(page.locator('[data-role="camera-block"]')).toHaveCount(0);
  gate.assertClean();
});

// shading#16, found live on PP 18.11.2026: an ARRANGE save that hid cam4 (its NDI preview live)
// panicked the WASM app ("Tried to access a reactive value that has already been disposed", then
// "RefCell already borrowed"). The preview <img>'s load event can land AFTER its block was removed
// (the frame decodes while the save removes the block); the handler must not touch the disposed
// block, and the removed <img> must keep no source (so no late load event). Deterministic: a frame
// on screen, keep the <img>, hide the camera.
test("a preview image whose load event lands after its block was hidden leaves a clean console (shading#16, PP 18.11)", async ({ page }) => {
  const gate = consoleGate(page);
  let hidden = false;
  await page.addInitScript(DISABLE_WS);
  await page.route("**/api/cameras", (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(makeFixture({ hasPreview: true, previewLive: true })) })
  );
  await page.route("**/api/layout", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(hidden ? { rev: 1, order: ["cam1"], hidden: ["cam1"] } : { rev: 0, order: [], hidden: [] }),
    })
  );
  await page.route(PREVIEW_URL_RE, (route) =>
    route.fulfill({ status: 200, contentType: "image/jpeg", headers: { "cache-control": "no-store" }, body: PREVIEW_JPEG })
  );
  await page.goto(`${PREVIEW_SVC}${NEXT}`);
  await waitForApp(page);
  const img = page.locator('[data-role="preview-img"]');
  await expect(img).toHaveClass(/ready/, { timeout: 12000 });
  // Keep the element itself: it stays a live object after Leptos removes it from the page.
  await img.evaluate((e) => {
    window.__keptPreviewImg = e;
  });
  hidden = true;
  await page.evaluate(() => window.shadingPush({ type: "layout", rev: 1, order: ["cam1"], hidden: ["cam1"] }));
  await expect(page.locator('[data-role="camera-block"]')).toHaveCount(0);
  await page.waitForTimeout(500);
  // The live bug: removing the block panicked the app right here.
  gate.assertClean();
  // The removed element must not load anything any more: its source is gone, so no late load
  // event can reach the removed block (a synthetic event would only hit Leptos's dropped listener).
  expect(await page.evaluate(() => window.__keptPreviewImg.hasAttribute("src")), "the removed preview keeps no source").toBe(false);
  await page.waitForTimeout(1000);
  // The app is still alive: the camera comes back on the next push and shows its preview again.
  hidden = false;
  await page.evaluate(() => window.shadingPush({ type: "layout", rev: 2, order: [], hidden: [] }));
  await expect(page.locator('[data-role="camera-block"]')).toHaveCount(1);
  await expect(img).toHaveClass(/ready/, { timeout: 12000 });
  gate.assertClean();
});

// shading#13 (owner ruling 15.11.2026, "1234567"): number order whatever the config order, and a
// block never moves when its camera goes offline or comes back. It is the default of ARRANGE.
test("cameras stand in number order whatever the config order, and never move on online/offline, console clean (shading#13)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.addInitScript(DISABLE_WS);
  await blockApi(page);
  await page.goto(NEXT);
  await waitForApp(page);

  const cam = (id, reachable, online) => ({
    id,
    label: id,
    transport: "cambox-relay",
    hasPreview: false,
    reachable,
    grabFps: null,
    grabFpsDesync: false,
    fpsSync: "unknown",
    control: !reachable ? "relay-unreachable" : online ? "ok" : "rest-off",
    state: reachable
      ? {
          online,
          camera: online ? "Blackmagic Pocket Cinema Camera 4K" : null,
          params: { apertureAv: null, apertureNorm: null, iso: null, kelvin: null, tint: null, shutter: null, fps100: null, sensorFps100: null, focusDistance: null },
          caps: null,
          fpsSupported: false,
          captureFps: null,
          version: "0.2.6",
          notApplied: [],
          offlineReason: online ? null : "rest-off",
        }
      : null,
  });
  const order = () => page.locator("#camera-grid > [data-id]").evaluateAll((els) => els.map((e) => e.dataset.id));
  const show = (cams) => pushState(page, { version: "0.2.7-e2e", cameras: cams });

  await show([cam("cam1", true, true), cam("cam4", true, true), cam("cam6", true, true), cam("cam7", true, true), cam("cam2", true, true)]);
  await expect.poll(order).toEqual(["cam1", "cam2", "cam4", "cam6", "cam7"]);

  await show([cam("cam1", true, false), cam("cam4", true, true), cam("cam6", false, false), cam("cam7", true, true), cam("cam2", true, true)]);
  await expect.poll(order).toEqual(["cam1", "cam2", "cam4", "cam6", "cam7"]);
  const block = (id) => page.locator(`#camera-grid > [data-id="${id}"]`);
  await expect(block("cam1")).toHaveClass(/disabled/);
  await expect(block("cam1").locator('[data-role="control-warn"]')).toBeVisible();
  await expect(block("cam6").locator('[data-role="control-warn"]')).toContainText("Relay nedostupný");

  await show([cam("cam1", true, true), cam("cam4", true, true), cam("cam6", true, true), cam("cam7", true, true), cam("cam2", true, true)]);
  await expect.poll(order).toEqual(["cam1", "cam2", "cam4", "cam6", "cam7"]);
  await expect(block("cam1")).not.toHaveClass(/disabled/);

  await show([cam("cam10", true, true), cam("spare", true, true), cam("cam7", true, true), cam("cam1", true, true)]);
  await expect.poll(order).toEqual(["cam1", "cam7", "cam10", "spare"]);
  gate.assertClean();
});

test("the align-to-grab button is offered only for a grab the camera lists, console clean (issue 1402)", async ({ page }) => {
  const gate = consoleGate(page);
  const puts = [];
  await page.addInitScript(DISABLE_WS);
  await blockApi(page, (p) => puts.push(p));
  await page.goto(NEXT);
  await waitForApp(page);

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
        params: { apertureAv: 4.78, apertureNorm: 2.0 / 3.0, iso: 400, kelvin: 5600, tint: 0, shutter: 120, fps100, sensorFps100, focusDistance: null },
        caps: { isoChoices: [100, 200, 400, 800], fNumberChoices: [2.8, 4.0, 5.2, 8.0], shutterChoices: [60, 100, 120, 125], fpsChoices: BMPCC_D006, fpsMin: 5, fpsMax: 60, kelvinMin: 2500, kelvinMax: 10000 },
        fpsSupported: true,
        captureFps: null,
        version: "0.2.7-e2e",
        notApplied: [],
      },
    };
    if (fpsAlignSettable !== undefined) v.fpsAlignSettable = fpsAlignSettable;
    return { version: "0.2.7-e2e", cameras: [v] };
  };
  const show = (opts) => pushState(page, view(opts));
  const fpsVal = page.locator('[data-role="fps-val"]');
  const warn = page.locator('[data-role="fps-warn"]');
  const btn = page.locator('[data-role="fps-set-grab"]');

  await show({ fps100: 6000, sensorFps100: 5000, grabFps: 60, fpsSync: "synced", fpsAlignSettable: false });
  await expect(fpsVal).toHaveText("60.00");
  await expect(page.locator('[data-role="fps-grab"]')).toHaveText("grab 60");
  await expect(warn).toBeHidden();
  await expect(btn).toBeHidden();

  await show({ fps100: 5000, sensorFps100: 5000, grabFps: 60, fpsSync: "mismatch", fpsAlignSettable: false });
  await expect(warn).toBeVisible();
  await expect(warn).toHaveText("⚠ kamera 50.00 ≠ grab 60");
  await expect(btn).toBeHidden();

  await show({ fps100: 5000, sensorFps100: 5000, grabFps: 60, fpsSync: "mismatch" });
  await expect(warn).toBeVisible();
  await expect(btn).toBeHidden();

  await show({ fps100: 2500, sensorFps100: 2500, grabFps: 50, fpsSync: "mismatch", fpsAlignSettable: true });
  await expect(btn).toBeVisible();
  await expect(btn).toHaveText("Zosúladiť s grab (50)");
  await btn.click();
  await expect.poll(() => puts.length).toBe(1);
  expect(puts[0].url).toContain("/api/cameras/cam3/params");
  expect(puts[0].body).toEqual({ fps: 50 });
  gate.assertClean();
});

const REGISTER_SVC = process.env.REGISTER_NEXT_SVC_BASE_URL || "http://127.0.0.1:8785";

test("a handheld that registers itself becomes reachable in the panel and takes a write, console clean (issue 808)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.goto(`${REGISTER_SVC}${NEXT}`);
  const online = page.locator('[data-role="online"]');
  const regLine = page.locator('[data-role="registration"]');
  const warn = page.locator('[data-role="control-warn"]');
  const dlg = page.locator('[data-role="control-info"]');

  // No announcement yet: no address for register:handheld-next, never a guess.
  await expect(online).toHaveText("relay offline", { timeout: 15000 });
  await expect(regLine).toBeHidden();
  await expect(warn).toContainText("Relay nedostupný");
  await warn.click();
  await expect(dlg.locator('[data-role="control-info-title"]')).toHaveText("Panel sa nespojí s relay pri kamere");
  await dlg.locator('[data-role="control-info-close"]').click();
  await expect(dlg).toBeHidden();

  const reg = await page.request.post(`${REGISTER_SVC}/api/relays/register`, { data: { id: "handheld-next", port: 8781 } });
  expect(reg.status()).toBe(200);
  expect(await reg.json()).toEqual({ id: "handheld-next", address: "127.0.0.1:8781" });

  await expect(online).toHaveText("online", { timeout: 15000 });
  await expect(warn).toBeHidden();
  await expect(regLine).toBeVisible();
  await expect(regLine).toContainText("handheld-next · 127.0.0.1:8781 · ohlásený pred");
  await expect(regLine).not.toHaveClass(/stale/);

  const before = (await recorded(page)).length;
  const kInc = page.locator('[data-role="kelvin-inc"]');
  await expect(kInc).toBeEnabled({ timeout: 15000 });
  await kInc.click();
  await expect.poll(async () => (await recorded(page)).length, { timeout: 10000 }).toBeGreaterThan(before);
  const got = (await recorded(page)).slice(before);
  expect(got.some((b) => b.kelvin === 5700), "kelvin + -> 5700 via the registration").toBeTruthy();
  gate.assertClean();
});

const CONTROL_CASES = [
  { control: "relay-unreachable", reachable: false, state: null, badge: "Relay nedostupný", title: "Panel sa nespojí s relay pri kamere", step: "Skontroluj, či je box pri kamere zapnutý a pripojený do siete" },
  { control: "relay-too-old", reachable: true, version: "0.1.2", badge: "Starý relay", title: "Relay pri kamere je príliš starý", cause: "verziu 0.1.2", step: "treba nasadiť nový relay" },
  { control: "no-camera-link", reachable: true, reason: "no-camera-link", badge: "Kamera nie je pripojená cez USB", title: "Relay nevidí kameru na USB", step: "skontroluj USB-C kábel" },
  { control: "camera-not-answering", reachable: true, reason: "camera-not-answering", badge: "Kamera neodpovedá", title: "Kamera je pripojená, ale neodpovedá", step: "Zapni na kamere Web media manager" },
  { control: "rest-off", reachable: true, reason: "rest-off", badge: "Zapni Web media manager na kamere", title: "Na kamere je vypnutý Web media manager", step: "Zapni na kamere Web media manager" },
  { control: "camera-offline", reachable: true, version: "0.2.5", badge: "Kamera offline", title: "Kamera sa nedá ovládať", step: "Web media manager" },
];

test("a camera that cannot be controlled shows a warning and a Slovak popup per cause, console clean (shading#14)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.addInitScript(DISABLE_WS);
  await blockApi(page);
  await page.goto(NEXT);
  await waitForApp(page);

  const view = (c) => ({
    version: "0.2.7-e2e",
    cameras: [
      {
        id: "cam7",
        label: "Cam 7",
        transport: "cambox-relay",
        hasPreview: false,
        reachable: c.reachable,
        grabFps: null,
        grabFpsDesync: false,
        fpsSync: "unknown",
        control: c.control,
        state:
          c.state === null
            ? null
            : { online: c.control === "ok", camera: null, params: {}, caps: null, fpsSupported: false, captureFps: null, version: c.version || "0.2.6", notApplied: [], offlineReason: c.reason || null },
      },
    ],
  });
  const warn = page.locator('[data-role="control-warn"]');
  const dlg = page.locator('[data-role="control-info"]');

  for (const c of CONTROL_CASES) {
    await pushState(page, view(c));
    await expect(warn, c.control).toBeVisible();
    await expect(warn, c.control).toContainText(c.badge);
    await warn.click();
    await expect(dlg, c.control).toBeVisible();
    await expect(dlg.locator('[data-role="control-info-title"]')).toHaveText(c.title);
    await expect(dlg.locator('[data-role="control-info-camera"]')).toHaveText("Cam 7");
    if (c.cause) await expect(dlg.locator('[data-role="control-info-cause"]')).toContainText(c.cause);
    await expect(dlg.locator('[data-role="control-info-step"]')).toContainText(c.step);
    await dlg.locator('[data-role="control-info-close"]').click();
    await expect(dlg).toBeHidden();
  }

  await pushState(page, view(CONTROL_CASES[4]));
  await warn.click();
  await expect(dlg.locator('[data-role="control-info-title"]')).toHaveText(CONTROL_CASES[4].title);
  await pushState(page, view(CONTROL_CASES[2]));
  await expect(dlg.locator('[data-role="control-info-title"]')).toHaveText(CONTROL_CASES[2].title);

  await pushState(page, view({ control: "ok", reachable: true }));
  await expect(warn).toBeHidden();
  await expect(dlg).toBeHidden();
  gate.assertClean();
});

test("the real service turns a relay's offline reason and an old relay into the panel warning, console clean (shading#14)", async ({ page }) => {
  const gate = consoleGate(page);
  const { STUB } = require("./next-helpers");
  const warn = page.locator('[data-role="control-warn"]');
  const dlg = page.locator('[data-role="control-info"]');
  try {
    await page.goto(NEXT);
    await expect(page.locator('[data-role="online"]')).toHaveText("online", { timeout: 15000 });
    await expect(warn).toBeHidden();

    await page.request.post(`${STUB}/__state`, { data: { online: false, offlineReason: "rest-off" } });
    await expect(warn).toContainText("Zapni Web media manager na kamere", { timeout: 15000 });
    await warn.click();
    await expect(dlg.locator('[data-role="control-info-step"]')).toContainText("Zapni na kamere Web media manager");
    await dlg.locator('[data-role="control-info-close"]').click();
    await expect(dlg).toBeHidden();

    await page.request.post(`${STUB}/__state`, { data: { offlineReason: null, version: "0.1.2" } });
    await expect(warn).toContainText("Starý relay", { timeout: 15000 });
    await warn.click();
    await expect(dlg.locator('[data-role="control-info-cause"]')).toContainText("verziu 0.1.2");
    await dlg.locator('[data-role="control-info-close"]').click();

    await page.request.post(`${STUB}/__reset`);
    await expect(warn).toBeHidden({ timeout: 15000 });
  } finally {
    await page.request.post(`${STUB}/__reset`);
  }
  gate.assertClean();
});

test("the PWA assets are served next to the panel, console clean (issue 1305)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.goto(NEXT);
  await waitForApp(page);
  const manifest = await page.request.get(`${NEXT}manifest.webmanifest`);
  expect(manifest.status()).toBe(200);
  expect(manifest.headers()["content-type"]).toContain("application/manifest+json");
  const m = await manifest.json();
  expect(m.lang).toBe("sk");
  expect(m.start_url).toBe("./");
  const sw = await page.request.get(`${NEXT}sw.js`);
  expect(sw.status()).toBe(200);
  expect(await sw.text()).not.toMatch(/caches\.|cache\.put/);
  for (const icon of ["icons/icon-192.png", "icons/icon-512.png", "icons/favicon.svg"]) {
    expect((await page.request.get(`${NEXT}${icon}`)).status(), icon).toBe(200);
  }
  await expect(page.locator('link[rel="manifest"]')).toHaveAttribute("href", "manifest.webmanifest");
  gate.assertClean();
});
