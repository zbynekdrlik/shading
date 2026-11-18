// @ts-check
// Real-browser E2E of the panel's theme v4b + version D card sizing (shading#24), served by the real
// service at /: the layout and mode pills (and their per-device memory), the card widths of each
// layout, the in-card scaling, the EXP/SYS panes, the value that opens the slider, the min/max
// overlay at a range end, and no layout shift on a push, a tally or a pane switch. Fixtures go in
// through `window.shadingPush`; every test ends on a clean console.
const { test, expect } = require("@playwright/test");
const {
  PANEL,
  consoleGate,
  neutraliseServiceWorker,
  DISABLE_WS,
  blockApi,
  waitForApp,
  pushState,
  makeFixture,
} = require("./next-helpers");

test.beforeEach(async ({ page }) => {
  await neutraliseServiceWorker(page);
});

/** An aggregate of `n` online cameras cam1..camN (the cam1 fixture, renumbered). */
function cams(n, opts = {}) {
  const one = makeFixture(opts).cameras[0];
  const cameras = [];
  for (let i = 1; i <= n; i++) {
    cameras.push({ ...JSON.parse(JSON.stringify(one)), id: `cam${i}`, label: `Cam ${i}` });
  }
  return { version: "0.2.10-e2e", cameras };
}

/** The panel on a fixture of `n` cameras at the viewport `w`x`h`, the service out of the loop. */
async function openWith(page, n, w = 1920, h = 1080, puts = null) {
  await page.setViewportSize({ width: w, height: h });
  await page.addInitScript(DISABLE_WS);
  await blockApi(page, puts ? (p) => puts.push(p) : undefined);
  await page.goto(PANEL);
  await waitForApp(page);
  await pushState(page, cams(n));
  await expect(page.locator('[data-role="camera-block"]')).toHaveCount(n);
}

/** Every card's rect, and how many lie whole / cut inside the window. */
async function cardsInView(page) {
  return page.evaluate(() => {
    const w = document.documentElement.clientWidth;
    const rects = [...document.querySelectorAll('[data-role="camera-block"]')].map((el) => el.getBoundingClientRect());
    return {
      w,
      widths: rects.map((r) => r.width),
      tops: rects.map((r) => r.top),
      whole: rects.filter((r) => r.left >= 0 && r.right <= w + 0.5).length,
      partial: rects.filter((r) => r.left < w - 0.5 && r.right > w + 0.5).length,
      pageScrollX: document.documentElement.scrollWidth > document.documentElement.clientWidth,
    };
  });
}

/** The rects of every card and of every control row inside them (layout-shift probe). */
async function geometry(page) {
  return page.evaluate(() =>
    [...document.querySelectorAll('[data-role="camera-block"], [data-role="camera-block"] .row, [data-role="camera-block"] .rows')].map((el) => {
      const r = el.getBoundingClientRect();
      return [Math.round(r.left * 10) / 10, Math.round(r.top * 10) / 10, Math.round(r.width * 10) / 10, Math.round(r.height * 10) / 10];
    })
  );
}

test("the layout pill: 4+4, exactly 5 whole cards, 4 1/3 in a row, remembered per device, console clean (shading#24)", async ({ page }) => {
  const gate = consoleGate(page);
  await openWith(page, 8);
  const html = page.locator("html");

  // The default is the 4+4 grid: two rows of four, each card at --card-max (460 px) on 1920 px.
  await expect(html).toHaveAttribute("data-layout", "grid");
  await expect(page.locator('[data-role="layout-grid"]')).toHaveAttribute("aria-pressed", "true");
  let v = await cardsInView(page);
  expect(new Set(v.tops.slice(0, 4)).size, "the first four share a row").toBe(1);
  expect(v.tops[4]).toBeGreaterThan(v.tops[0]);
  for (const w of v.widths) expect(w).toBeCloseTo(460, 0);

  // "5": exactly five whole cards, the sixth starts past the window's edge (no partial sixth).
  await page.locator('[data-role="layout-five"]').click();
  await expect(html).toHaveAttribute("data-layout", "five");
  v = await cardsInView(page);
  expect(v.whole, JSON.stringify(v)).toBe(5);
  expect(v.partial, JSON.stringify(v)).toBe(0);
  for (const w of v.widths) expect(w).toBeCloseTo((1920 - 16 - 5 * 14) / 5 + 0.1, 0);
  expect(v.pageScrollX, "the row scrolls inside the grid, never the page").toBe(false);
  expect(await page.locator("#camera-grid").evaluate((el) => getComputedStyle(el).scrollSnapType)).toBe("none");

  // "8 v rade": four whole cards and a third of the fifth.
  await page.locator('[data-role="layout-row"]').click();
  v = await cardsInView(page);
  expect(v.whole).toBe(4);
  expect(v.partial).toBe(1);

  // Remembered on this device: a reload comes back in "5".
  await page.locator('[data-role="layout-five"]').click();
  expect(await page.evaluate(() => localStorage.getItem("shading.layout"))).toBe("five");
  await page.reload();
  await waitForApp(page);
  await pushState(page, cams(8));
  await expect(html).toHaveAttribute("data-layout", "five");
  await expect(page.locator('[data-role="layout-five"]')).toHaveAttribute("aria-pressed", "true");
  v = await cardsInView(page);
  expect(v.whole).toBe(5);
  gate.assertClean();
});

test("version D: cards keep their min/max width and everything inside scales with the card, console clean (shading#24)", async ({ page }) => {
  const gate = consoleGate(page);
  const ratios = [];
  const probe = async () =>
    page.evaluate(() => {
      const card = document.querySelector('[data-role="camera-block"]');
      const val = card.querySelector('[data-role="kelvin-val"]');
      const btn = card.querySelector('[data-role="kelvin-inc"]');
      // The card's inner width: cqw measures the container's content box (inside the 2 px border).
      const w = card.clientWidth;
      return { w, val: parseFloat(getComputedStyle(val).fontSize) / w, btn: btn.getBoundingClientRect().height / w };
    });

  // "8 v rade" on a 4000 px wall: every card capped at 460 px, all eight fit one screen.
  await openWith(page, 8, 4000, 2000);
  await page.locator('[data-role="layout-row"]').click();
  let v = await cardsInView(page);
  expect(v.whole).toBe(8);
  for (const w of v.widths) expect(w).toBeCloseTo(460, 0);
  ratios.push(await probe());

  // "5" on 1920: 367 px cards.
  await page.setViewportSize({ width: 1920, height: 1080 });
  await page.locator('[data-role="layout-five"]').click();
  ratios.push(await probe());

  // "5" on a 1366 px notebook: still exactly five whole cards (Marcel, version D: card-min 250).
  await page.setViewportSize({ width: 1366, height: 900 });
  v = await cardsInView(page);
  expect(v.whole, JSON.stringify(v)).toBe(5);
  expect(v.partial, JSON.stringify(v)).toBe(0);

  // "8 v rade" on a 1000 px window: the card would be 210 px, so it holds its 250 px minimum.
  await page.setViewportSize({ width: 1000, height: 900 });
  await page.locator('[data-role="layout-row"]').click();
  v = await cardsInView(page);
  for (const w of v.widths) expect(w).toBeCloseTo(250, 0);
  ratios.push(await probe());

  // The value and the +/- button keep the same share of the card's width at every size.
  expect(ratios[0].w).not.toBeCloseTo(ratios[2].w, 0);
  for (const r of ratios) {
    expect(r.val, JSON.stringify(ratios)).toBeCloseTo(26 / 367, 3);
    expect(r.btn, JSON.stringify(ratios)).toBeCloseTo(56 / 367, 3);
  }
  // Nothing wraps or overflows in the narrowest card: label, value, name and state stay one line.
  const overflow = await page.evaluate(() =>
    [...document.querySelectorAll(".mid .lab, .mid .val, .head .name, .head .state, .reg")]
      .filter((el) => el.offsetParent !== null)
      .filter((el) => el.getBoundingClientRect().height > parseFloat(getComputedStyle(el).lineHeight) * 1.6 || el.scrollWidth > el.clientWidth + 1)
      .map((el) => el.className + ":" + el.textContent)
  );
  expect(overflow).toEqual([]);
  gate.assertClean();
});

test("the mode pill: dark by default, a muted light mode, the outline only in the dark, remembered, console clean (shading#24)", async ({ page }) => {
  const gate = consoleGate(page);
  await openWith(page, 1);
  const html = page.locator("html");
  const bg = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  const valStyle = () =>
    page.locator('[data-role="kelvin-val"]').evaluate((el) => {
      const s = getComputedStyle(el);
      return { shadow: s.textShadow, size: s.fontSize, weight: s.fontWeight, spacing: s.letterSpacing, color: s.color };
    });

  await expect(html).toHaveAttribute("data-mode", "dark");
  expect(await bg()).toBe("rgb(15, 18, 22)");
  const dark = await valStyle();
  expect(dark).toMatchObject({ weight: "500", spacing: "1px", color: "rgb(205, 213, 222)" });
  expect(dark.shadow).toContain("rgb(0, 0, 0) 1.5px 0px 0px");
  const glyph = await page.locator('[data-role="kelvin-inc"]').evaluate((el) => getComputedStyle(el).textShadow);
  expect(glyph).toContain("1.5px");

  await page.locator('[data-role="mode-light"]').click();
  await expect(html).toHaveAttribute("data-mode", "light");
  expect(await bg()).toBe("rgb(185, 193, 203)");
  expect((await valStyle()).shadow).toBe("none");

  await page.reload();
  await waitForApp(page);
  await expect(html).toHaveAttribute("data-mode", "light");
  await expect(page.locator('[data-role="mode-light"]')).toHaveAttribute("aria-pressed", "true");
  expect(await page.evaluate(() => localStorage.getItem("shading.mode"))).toBe("light");
  gate.assertClean();
});

test("a blocked localStorage leaves the defaults and a clean console (shading#24)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.addInitScript(() => {
    Object.defineProperty(window, "localStorage", {
      get() {
        throw new DOMException("blocked", "SecurityError");
      },
    });
  });
  await openWith(page, 2);
  await expect(page.locator("html")).toHaveAttribute("data-layout", "grid");
  await expect(page.locator("html")).toHaveAttribute("data-mode", "dark");
  await page.locator('[data-role="layout-five"]').click();
  await expect(page.locator("html")).toHaveAttribute("data-layout", "five");
  gate.assertClean();
});

test("EXP/SYS: a pane switch keeps the card's height, each pane holds its controls, console clean (shading#24)", async ({ page }) => {
  const gate = consoleGate(page);
  await openWith(page, 1);
  const card = page.locator('[data-role="camera-block"]');
  const h0 = (await card.boundingBox()).height;

  await expect(page.locator('[data-role="pane-exp"]')).toHaveAttribute("aria-selected", "true");
  for (const role of ["fnum", "iso-val", "kelvin-val", "tint-val"]) await expect(page.locator(`[data-role="${role}"]`)).toBeVisible();
  for (const role of ["shutter-val", "fps-val", "auto-wb"]) await expect(page.locator(`[data-role="${role}"]`)).toBeHidden();

  await page.locator('[data-role="pane-sys"]').click();
  await expect(page.locator('[data-role="pane-sys"]')).toHaveAttribute("aria-selected", "true");
  for (const role of ["shutter-val", "fps-val", "auto-wb"]) await expect(page.locator(`[data-role="${role}"]`)).toBeVisible();
  await expect(page.locator('[data-role="fnum"]')).toBeHidden();
  expect((await card.boundingBox()).height).toBe(h0);

  await page.locator('[data-role="pane-exp"]').click();
  expect((await card.boundingBox()).height).toBe(h0);
  gate.assertClean();
});

test("a tap on the value opens its slider; the release writes once; a second tap closes it, console clean (shading#24)", async ({ page }) => {
  const gate = consoleGate(page);
  const puts = [];
  await openWith(page, 1, 1920, 1080, puts);
  const row = page.locator('.row[data-row="kelvin"]');
  const val = page.locator('[data-role="kelvin-val"]');
  const slider = page.locator('[data-role="kelvin-slider"]');

  await expect(slider).toBeHidden();
  await val.click();
  await expect(row).toHaveClass(/open/);
  await expect(val).toHaveAttribute("aria-expanded", "true");
  await expect(slider).toBeVisible();
  expect(Number(await slider.inputValue())).toBe(5600);
  // The open value is highlighted with the accent.
  expect(await val.evaluate((el) => getComputedStyle(el).backgroundColor)).toBe("rgb(91, 155, 255)");

  await slider.fill("6400");
  await expect.poll(() => puts.length).toBe(1);
  expect(puts[0].body).toEqual({ kelvin: 6400 });
  await expect(val).toHaveText("6400 K");

  await val.click();
  await expect(row).not.toHaveClass(/open/);
  await expect(slider).toBeHidden();
  // No text was selected by the taps (an app, not a web page).
  expect(await page.evaluate(() => String(window.getSelection()))).toBe("");
  // WebKit reports it only as the prefixed property.
  const select = (el) => getComputedStyle(el).userSelect || getComputedStyle(el).webkitUserSelect;
  expect(await val.evaluate(select)).toBe("none");
  expect(await page.locator(".reg").first().evaluate(select)).toBe("text");
  gate.assertClean();
});

// Ported from the old panel's spec (the slider lived only there): a release toward a lens end the
// iris already reached writes nothing and the thumb goes back; away from it, it writes.
test("the aperture slider never writes past a reached lens end (irisRangePos), console clean (shading#6, shading#22)", async ({ page }) => {
  const gate = consoleGate(page);
  const puts = [];
  await page.addInitScript(DISABLE_WS);
  await blockApi(page, (p) => puts.push(p));
  await page.goto(PANEL);
  await waitForApp(page);
  const fnum = page.locator('[data-role="fnum"]');
  const slider = page.locator('[data-role="aperture-slider"]');

  // Wide open at f/4.0 (grid index 1 of [2.8, 4.0, 5.2, 8.0]).
  await pushState(page, makeFixture({ apertureAv: 4.0, apertureNorm: 1 / 3, irisRangePos: 0.0 }));
  await expect(fnum).toHaveText("f/4.0");
  await fnum.click();
  await expect(slider).toBeVisible();
  expect(Number(await slider.inputValue())).toBe(1);
  await slider.fill("0");
  await expect.poll(async () => Number(await slider.inputValue())).toBe(1);
  await expect(fnum).toHaveText("f/4.0");
  expect(puts, "no write past the wide end").toEqual([]);

  await slider.fill("3");
  await expect.poll(() => puts.length).toBe(1);
  expect(puts[0].body).toEqual({ apertureNorm: 1 });
  await expect(fnum).toHaveText("f/8.0");

  // Closed: a release further closed sends nothing.
  puts.length = 0;
  await pushState(page, makeFixture({ apertureAv: 6.0, apertureNorm: 1, irisRangePos: 1.0 }));
  await expect(fnum).toHaveText("f/8.0");
  await slider.fill("2");
  await expect.poll(() => puts.length).toBe(1);
  expect(puts[0].body.apertureNorm).toBeCloseTo(2 / 3, 6);
  gate.assertClean();
});

test("a range end lays min/max as a sibling overlay over the dead button, console clean (shading#24)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.addInitScript(DISABLE_WS);
  await blockApi(page);
  await page.goto(PANEL);
  await waitForApp(page);
  await pushState(page, makeFixture({ iso: 100, kelvin: 10000 }));

  const isoDec = page.locator('[data-role="iso-dec"]');
  const isoMin = page.locator('.row[data-row="iso"] [data-role="dec-end"]');
  await expect(isoDec).toBeDisabled();
  await expect(isoMin).toHaveText("min");
  await expect(isoMin).toBeVisible();
  await expect(page.locator('.row[data-row="iso"] [data-role="inc-end"]')).toBeHidden();
  const kMax = page.locator('.row[data-row="kelvin"] [data-role="inc-end"]');
  await expect(kMax).toHaveText("max");
  await expect(page.locator('[data-role="kelvin-inc"]')).toBeDisabled();

  // A sibling of the button (never its child), laid exactly over it and centred, taking no tap.
  const geo = await isoMin.evaluate((lab) => {
    const btn = lab.parentElement.querySelector("button");
    const a = lab.getBoundingClientRect();
    const b = btn.getBoundingClientRect();
    const range = document.createRange();
    range.selectNodeContents(lab);
    const t = range.getBoundingClientRect();
    return {
      child: btn.contains(lab),
      sameBox: Math.abs(a.left - b.left) < 0.5 && Math.abs(a.top - b.top) < 0.5 && Math.abs(a.width - b.width) < 0.5 && Math.abs(a.height - b.height) < 0.5,
      dx: Math.abs((t.left + t.right) / 2 - (b.left + b.right) / 2),
      dy: Math.abs((t.top + t.bottom) / 2 - (b.top + b.bottom) / 2),
      events: getComputedStyle(lab).pointerEvents,
    };
  });
  expect(geo.child).toBe(false);
  expect(geo.sameBox).toBe(true);
  expect(geo.dx).toBeLessThan(1);
  expect(geo.dy).toBeLessThan(1);
  expect(geo.events).toBe("none");

  // Back inside the range: the overlay goes, the button works.
  await pushState(page, makeFixture({ iso: 400, kelvin: 5600 }));
  await expect(isoMin).toBeHidden();
  await expect(isoDec).toBeEnabled();
  gate.assertClean();
});

test("no layout shift: a push, a warning, a tally and a pane switch move no card and no row, console clean (shading#24)", async ({ page }) => {
  const gate = consoleGate(page);
  await openWith(page, 5);
  await page.locator('[data-role="layout-five"]').click();
  const before = await geometry(page);

  // A push: other values, one camera loses its relay (warning overlay, dashes), one registers.
  const agg = cams(5, { iso: 800, kelvin: 3200, tint: -12, apertureAv: 6.0 });
  agg.cameras[1].reachable = false;
  agg.cameras[1].state = null;
  agg.cameras[1].control = "relay-unreachable";
  agg.cameras[2].registration = { relayId: "cam3-shading", address: "10.77.8.75:8771", ageS: 3, announcingS: 4020, fresh: true };
  agg.cameras[3].grabFps = 60;
  agg.cameras[3].fpsSync = "mismatch";
  agg.cameras[3].fpsAlignSettable = true;
  agg.cameras[3].state.params.fps100 = 5000;
  await pushState(page, agg);
  await expect(page.locator('[data-id="cam2"] [data-role="control-warn"]')).toBeVisible();
  await expect(page.locator('[data-id="cam3"] [data-role="registration"]')).toHaveText("cam3-shading · 10.77.8.75:8771 · ↻ 3s · ▲ 1h07m");
  await expect(page.locator('[data-id="cam4"] [data-role="fps-set-grab"]')).toBeVisible();
  expect(await geometry(page)).toEqual(before);

  // The tally recolours only (shading#25): PGM on one card, PVW on another.
  agg.cameras[0].tally = "pgm";
  agg.cameras[2].tally = "pvw";
  await pushState(page, agg);
  await expect(page.locator('[data-id="cam1"] [data-role="tally"]')).toHaveText("PGM");
  expect(await page.locator('[data-role="camera-block"]').first().evaluate((el) => getComputedStyle(el).borderTopColor)).toBe("rgb(229, 52, 58)");
  expect(await geometry(page)).toEqual(before);

  // A pane switch.
  await page.locator('[data-role="pane-sys"]').first().click();
  expect(await geometry(page)).toEqual(before);
  gate.assertClean();
});

test("on a phone the bar is title + state + ☰, the controls open in an overlay menu, console clean (shading#24 version D)", async ({ page }) => {
  const gate = consoleGate(page);
  await openWith(page, 3, 390, 844);
  const burger = page.locator('[data-role="menu-key"]');
  const five = page.locator('[data-role="layout-five"]');

  await expect(burger).toBeVisible();
  await expect(burger).toHaveAttribute("aria-expanded", "false");
  await expect(burger).toHaveAttribute("aria-controls", "top-menu");
  await expect(page.locator('[data-role="version"]')).toBeHidden();
  for (const role of ["layout-pill", "mode-pill", "arrange-key"]) await expect(page.locator(`[data-role="${role}"]`)).toBeHidden();
  const noScrollX = () => page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth);
  expect(await noScrollX()).toBe(true);

  // Opening the menu moves no card (an overlay over them).
  const before = await geometry(page);
  await burger.click();
  await expect(burger).toHaveAttribute("aria-expanded", "true");
  await expect(five).toBeVisible();
  await expect(page.locator('[data-role="mode-pill"]')).toBeVisible();
  const key = page.locator('[data-role="arrange-key"]');
  await expect(key).toBeVisible();
  expect(await key.evaluate((el) => getComputedStyle(el, "::after").content)).toBe('" ARRANGE"');
  expect(await geometry(page)).toEqual(before);
  expect(await noScrollX()).toBe(true);

  // "5" chosen in the menu applies the layout (a phone shows one card a screen, the row scrolls).
  await five.click();
  await expect(page.locator("html")).toHaveAttribute("data-layout", "five");
  await expect(five).toHaveAttribute("aria-pressed", "true");
  expect(await noScrollX()).toBe(true);
  await burger.click();
  await expect(five).toBeHidden();

  // Above 700 px nothing changed: no burger, the pills sit in the bar.
  await page.setViewportSize({ width: 1280, height: 900 });
  await expect(burger).toBeHidden();
  await expect(five).toBeVisible();
  await expect(page.locator('[data-role="version"]')).toBeVisible();
  gate.assertClean();
});

test("a phone without a saved layout starts in a row; a saved choice wins after a reload, console clean (shading#24 version D)", async ({ page }) => {
  const gate = consoleGate(page);
  await openWith(page, 3, 390, 844);
  const html = page.locator("html");
  expect(await page.evaluate(() => localStorage.getItem("shading.layout"))).toBeNull();
  await expect(html).toHaveAttribute("data-layout", "row");

  await page.locator('[data-role="menu-key"]').click();
  await page.locator('[data-role="layout-grid"]').click();
  await expect(html).toHaveAttribute("data-layout", "grid");
  await page.reload();
  await waitForApp(page);
  await expect(html).toHaveAttribute("data-layout", "grid");

  // A wide screen without a saved layout starts in the grid.
  await page.evaluate(() => localStorage.removeItem("shading.layout"));
  await page.setViewportSize({ width: 1920, height: 1080 });
  await page.reload();
  await waitForApp(page);
  await expect(html).toHaveAttribute("data-layout", "grid");
  gate.assertClean();
});

test("the top bar never scrolls the page sideways at any width, console clean (shading#24 version D)", async ({ page }) => {
  const gate = consoleGate(page);
  await openWith(page, 8, 1920, 1080);
  for (const w of [390, 701, 800, 1000, 1280, 1920]) {
    await page.setViewportSize({ width: w, height: 900 });
    const m = await page.evaluate(() => {
      const bar = document.querySelector(".bar");
      return {
        page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
        bar: bar.scrollWidth - bar.clientWidth,
        barH: bar.getBoundingClientRect().height,
      };
    });
    expect(m.page, `page at ${w}px`).toBeLessThanOrEqual(0);
    expect(m.bar, `bar at ${w}px`).toBeLessThanOrEqual(0);
    expect(m.barH, `one-line bar at ${w}px`).toBeLessThan(80);
  }
  gate.assertClean();
});

test("a slider dragged away and back (no change event) follows the camera again, console clean (shading#24)", async ({ page }) => {
  const gate = consoleGate(page);
  const puts = [];
  await openWith(page, 1, 1920, 1080, puts);
  await page.locator('[data-role="kelvin-val"]').click();
  const slider = page.locator('[data-role="kelvin-slider"]');
  await expect(slider).toBeVisible();
  // A drag away and back to the start: `input` events, a release, and NO `change` (the value
  // ended where it began), exactly as the browser reports it.
  await slider.evaluate((el) => {
    el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    el.value = "7000";
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.value = "5600";
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new PointerEvent("pointerup", { bubbles: true }));
  });
  // A push of a new value moves the thumb: the finished drag no longer holds it.
  await pushState(page, cams(1, { kelvin: 9000 }));
  await expect(page.locator('[data-role="kelvin-val"]')).toHaveText("9000 K");
  await expect.poll(async () => Number(await slider.inputValue())).toBe(9000);
  expect(puts, "a drag that ends where it started writes nothing").toEqual([]);
  gate.assertClean();
});

/** Each card's first control row, measured from the card's own top (the status-line probe). */
async function firstRowOffsets(page) {
  return page.evaluate(() =>
    Object.fromEntries(
      [...document.querySelectorAll('[data-role="camera-block"]')].map((card) => {
        const row = card.querySelector(".row");
        const offset = row.getBoundingClientRect().top - card.getBoundingClientRect().top;
        return [card.dataset.id, Math.round(offset * 10) / 10];
      })
    )
  );
}

test("the status line is always one line: online and offline cards put their first control row at the same offset, console clean (shading#28)", async ({ page }) => {
  const gate = consoleGate(page);
  await openWith(page, 3);
  const reg = { relayId: "cam1", address: "10.77.8.61:8771", ageS: 2, announcingS: 4080, fresh: true };
  const agg = cams(3);
  agg.cameras[0].registration = reg;
  agg.cameras[1].reachable = false;
  agg.cameras[1].state = null;
  agg.cameras[1].control = "relay-unreachable";
  agg.cameras[1].address = "10.77.8.64:8771";
  agg.cameras[2].reachable = false;
  agg.cameras[2].state = null;
  agg.cameras[2].control = "relay-unreachable";
  await pushState(page, agg);
  await expect(page.locator('[data-id="cam1"] [data-role="registration"]')).toHaveText("cam1 · 10.77.8.61:8771 · ↻ 2s · ▲ 1h08m");
  await expect(page.locator('[data-id="cam2"] [data-role="registration"]')).toHaveText("10.77.8.64:8771 · bez ohlásenia");
  await expect(page.locator('[data-id="cam3"] [data-role="registration"]')).toHaveText("—");
  await expect(page.locator('[data-id="cam2"] [data-role="registration"]')).toBeVisible();
  // The line's height never depends on its glyphs (the ↻/▲ fallback font is taller on the
  // operators' devices): an explicit line-height and height, equal in every card.
  const regBoxes = await page.evaluate(() =>
    [...document.querySelectorAll('[data-role="camera-block"] .reg')].map((el) => {
      const cs = getComputedStyle(el);
      return [cs.lineHeight, cs.height];
    })
  );
  expect(regBoxes[0][0]).not.toBe("normal");
  expect(regBoxes[0][1]).not.toBe("auto");
  expect(regBoxes.every((b) => b[0] === regBoxes[0][0] && b[1] === regBoxes[0][1])).toBeTruthy();
  const offsets = await firstRowOffsets(page);
  expect(offsets.cam2).toBe(offsets.cam1);
  expect(offsets.cam3).toBe(offsets.cam1);
  const before = await geometry(page);

  // The relays swap: cam1 drops out (no registration any more), cam2 comes back registered.
  const flipped = cams(3);
  flipped.cameras[0].reachable = false;
  flipped.cameras[0].state = null;
  flipped.cameras[0].control = "relay-unreachable";
  flipped.cameras[1].registration = { ...reg, relayId: "cam2", address: "10.77.8.64:8771" };
  flipped.cameras[2].reachable = false;
  flipped.cameras[2].state = null;
  flipped.cameras[2].control = "relay-unreachable";
  await pushState(page, flipped);
  await expect(page.locator('[data-id="cam1"] [data-role="registration"]')).toHaveText("—");
  await expect(page.locator('[data-id="cam2"] [data-role="registration"]')).toHaveText("cam2 · 10.77.8.64:8771 · ↻ 2s · ▲ 1h08m");
  expect(await geometry(page)).toEqual(before);
  gate.assertClean();
});

test("the PGM/PVW tally follows the push: red border + tint + PGM badge, green PVW edge, nothing moves, console clean (shading#25)", async ({ page }) => {
  const gate = consoleGate(page);
  await openWith(page, 4);
  const card = (id) => page.locator(`[data-id="${id}"]`);
  const look = (id) =>
    card(id).evaluate((el) => {
      const cs = getComputedStyle(el);
      return { border: cs.borderTopColor, width: cs.borderTopWidth, bg: cs.backgroundColor };
    });
  const plain = await look("cam1");
  expect(plain.width).toBe("2px");
  const before = await geometry(page);

  const agg = cams(4);
  agg.cameras[0].tally = "pgm";
  agg.cameras[2].tally = "pvw";
  await pushState(page, agg);
  await expect(card("cam1")).toHaveClass(/\bpgm\b/);
  await expect(card("cam1").locator('[data-role="tally"]')).toHaveText("PGM");
  await expect(card("cam3")).toHaveClass(/\bpvw\b/);
  await expect(card("cam3").locator('[data-role="tally"]')).toHaveText("PVW");
  await expect(card("cam2").locator('[data-role="tally"]')).toHaveCount(0);
  const pgm = await look("cam1");
  expect(pgm.border).toBe("rgb(229, 52, 58)");
  expect(pgm.width).toBe("2px");
  expect(pgm.bg, "the PGM card is tinted").not.toBe(plain.bg);
  const pvw = await look("cam3");
  expect(pvw.border).toBe("rgb(47, 174, 92)");
  expect(pvw.bg, "PVW is an edge only").toBe(plain.bg);
  expect(await geometry(page)).toEqual(before);

  // A cut: cam3 goes live, cam1 to preview.
  agg.cameras[0].tally = "pvw";
  agg.cameras[2].tally = "pgm";
  await pushState(page, agg);
  await expect(card("cam3")).toHaveClass(/\bpgm\b/);
  await expect(card("cam1")).toHaveClass(/\bpvw\b/);
  await expect(card("cam1")).not.toHaveClass(/\bpgm\b/);
  expect((await look("cam3")).bg).toBe(pgm.bg);
  expect(await geometry(page)).toEqual(before);

  // The tally goes away (OBS lost): every card plain again.
  for (const c of agg.cameras) c.tally = null;
  await pushState(page, agg);
  await expect(page.locator('[data-role="tally"]')).toHaveCount(0);
  expect(await look("cam3")).toEqual(plain);
  expect(await geometry(page)).toEqual(before);
  gate.assertClean();
});
