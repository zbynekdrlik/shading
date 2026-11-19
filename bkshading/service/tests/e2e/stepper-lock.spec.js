// @ts-check
// shading#29 (PP operator Marcel, Discord 'Grading NEW', 18.11.2026): the +/- steppers against the
// real service and a stub relay whose write answers late (`POST /__write {delayMs}`). A press shows
// the expected value at once; a double click writes once; while the write is unanswered both
// buttons of the row ignore presses and a thin bar runs under the value; the answer ends the bar
// and the lock; a refused write reverts the value and flashes it red.
//
// The service on :8787 polls its OWN stub relay (:8786, --apply-writes: a PUT changes the served
// params and answers the new state, which the service pushes at once). fresh-view.spec.js shares
// that pair; every test here resets the stub in `finally`.
const { test, expect } = require("@playwright/test");
const { consoleGate, neutraliseServiceWorker, waitForApp } = require("./next-helpers");

const SVC = "http://127.0.0.1:8787";
const STUB = "http://127.0.0.1:8786";

test.beforeEach(async ({ page }) => {
  await neutraliseServiceWorker(page);
});

async function writes(page) {
  return (await (await page.request.get(`${STUB}/__recorded`)).json()).length;
}

// Records every distinct text of the aperture value, every 10 ms, inside the page. The current
// text is recorded at once (synchronously), so a press right after this never beats the first
// tick (CI run 44429212413 lost the start value that way).
async function startFnumSampler(page) {
  await page.evaluate(() => {
    const w = /** @type {any} */ (window);
    const read = () => {
      const el = document.querySelector('[data-role="fnum"]');
      const t = el ? el.textContent : null;
      if (w.__fnum[w.__fnum.length - 1] !== t) w.__fnum.push(t);
    };
    w.__fnum = [];
    read();
    w.__fnumTimer = setInterval(read, 10);
  });
}

async function stopFnumSampler(page) {
  return page.evaluate(() => {
    const w = /** @type {any} */ (window);
    clearInterval(w.__fnumTimer);
    return w.__fnum;
  });
}

async function open(page) {
  await page.request.post(`${STUB}/__reset`);
  await page.goto(`${SVC}/`);
  await waitForApp(page);
  await expect(page.locator('[data-role="iso-val"]')).toHaveText("400");
  await expect(page.locator('[data-role="kelvin-val"]')).toHaveText("5600 K");
}

test("a press shows the next value at once and locks the row with a bar until the answer, console clean (shading#29)", async ({ page }) => {
  const gate = consoleGate(page);
  try {
    await open(page);
    await page.request.post(`${STUB}/__write`, { data: { delayMs: 2500 } });
    const val = page.locator('[data-role="kelvin-val"]');
    const bar = page.locator('[data-role="kelvin-pending"]');
    const inc = page.locator('[data-role="kelvin-inc"]');
    const dec = page.locator('[data-role="kelvin-dec"]');
    await expect(bar).toBeHidden();
    const before = await writes(page);

    await inc.click();
    // At once: the write answers only 2.5 s later.
    await expect(val).toHaveText("5700 K", { timeout: 500 });
    await expect(bar).toBeVisible({ timeout: 500 });
    await expect(inc).toHaveAttribute("aria-busy", "true");
    // The buttons look the same (no disabled state), the bar sits under the value, inside the row.
    await expect(inc).toBeEnabled();
    await expect(dec).toBeEnabled();
    const v = await val.boundingBox();
    const b = await bar.boundingBox();
    expect(b && v && b.y >= v.y + v.height - 1, "the bar is under the value").toBeTruthy();
    expect(b && v && Math.abs(b.x + b.width / 2 - (v.x + v.width / 2)) < 2, "centred").toBeTruthy();

    // Presses during the lock, spaced past the double-click window, on both buttons: nothing.
    for (const btn of [dec, dec, inc]) {
      await page.waitForTimeout(400);
      await btn.click();
    }
    await expect(val).toHaveText("5700 K");
    expect(await writes(page), "one write while locked").toBe(before + 1);

    // The answer: the bar goes, the value is the camera's, the row is free again.
    await expect(bar).toBeHidden({ timeout: 5000 });
    await expect(inc).not.toHaveAttribute("aria-busy", "true");
    await expect(val).toHaveText("5700 K");
    await expect(val).not.toHaveClass(/flash/);
    await page.request.post(`${STUB}/__write`, { data: { delayMs: 0 } });
    await dec.click();
    await expect(val).toHaveText("5600 K");
    await expect.poll(() => writes(page)).toBe(before + 2);
  } finally {
    await page.request.post(`${STUB}/__reset`);
  }
  gate.assertClean();
});

test("a double click sends ONE write, console clean (shading#29)", async ({ page }) => {
  const gate = consoleGate(page);
  try {
    await open(page);
    const before = await writes(page);
    await page.locator('[data-role="kelvin-inc"]').dblclick();
    await expect(page.locator('[data-role="kelvin-val"]')).toHaveText("5700 K");
    // Long past both the 300 ms window and the stub's instant answer.
    await page.waitForTimeout(1500);
    expect(await writes(page)).toBe(before + 1);
    await expect(page.locator('[data-role="kelvin-val"]')).toHaveText("5700 K");
  } finally {
    await page.request.post(`${STUB}/__reset`);
  }
  gate.assertClean();
});

// The lock alone already drops the second click of a fast double click; this one proves the 300 ms
// guard: the second press lands AFTER the first write's answer (the row is free again) but within
// 300 ms of the first press, so only the guard can drop it.
test("a second press after a fast answer but within 300 ms is ignored, console clean (shading#29)", async ({ page }) => {
  const gate = consoleGate(page);
  try {
    await open(page);
    const inc = page.locator('[data-role="kelvin-inc"]');
    const bar = page.locator('[data-role="kelvin-pending"]');
    const before = await writes(page);
    const t0 = Date.now();
    await inc.click();
    await expect(bar).toBeHidden({ timeout: 2000 });
    await expect(inc).not.toHaveAttribute("aria-busy", "true");
    const answeredAfter = Date.now() - t0;
    await inc.click();
    const secondAfter = Date.now() - t0;
    // Loud, never vacuous: if the answer was slower than the window the test proves nothing.
    expect(secondAfter, `the answer took ${answeredAfter} ms`).toBeLessThan(300);
    await page.waitForTimeout(800);
    expect(await writes(page), "the second press was dropped").toBe(before + 1);
    await expect(page.locator('[data-role="kelvin-val"]')).toHaveText("5700 K");
    // Past the window a press writes again.
    await inc.click();
    await expect.poll(() => writes(page)).toBe(before + 2);
  } finally {
    await page.request.post(`${STUB}/__reset`);
  }
  gate.assertClean();
});

test("a refused write reverts the value and flashes it red, console clean (shading#29)", async ({ page }) => {
  const gate = consoleGate(page);
  try {
    await open(page);
    await page.request.post(`${STUB}/__write`, { data: { delayMs: 800, refuse: true } });
    const val = page.locator('[data-role="kelvin-val"]');
    const bar = page.locator('[data-role="kelvin-pending"]');
    await page.locator('[data-role="kelvin-inc"]').click();
    await expect(val).toHaveText("5700 K", { timeout: 500 });
    await expect(bar).toBeVisible();
    // The camera refused: back to its value, red flash, the bar gone.
    await expect(val).toHaveText("5600 K", { timeout: 5000 });
    await expect(val).toHaveClass(/flash/);
    await expect(bar).toBeHidden();
    // The flash is brief.
    await expect(val).not.toHaveClass(/flash/, { timeout: 4000 });
  } finally {
    await page.request.post(`${STUB}/__reset`);
  }
  gate.assertClean();
});

// shading#17: the relay says WHY a press was not applied (`notAppliedReason`); the panel shows the
// reason's Slovak text in the note under the header and as the refused value's tooltip.
test("a refused press shows the relay's reason in the note and the tooltip, console clean (shading#17)", async ({ page }) => {
  const gate = consoleGate(page);
  const lensEnd = "Clona je na konci rozsahu objektívu";
  try {
    await open(page);
    await page.request.post(`${STUB}/__write`, { data: { refuse: true, reason: "lens-end" } });
    const val = page.locator('[data-role="fnum"]');
    await expect(val).toHaveText("f/5.2");
    await page.locator('[data-role="aperture-inc"]').click();
    await expect(page.locator('[data-role="arrange-note"]')).toHaveText(lensEnd, { timeout: 5000 });
    await expect(val).toHaveClass(/flash/);
    await expect(val).toHaveAttribute("title", lensEnd);
    await expect(page.locator('[data-role="aperture-inc"]')).toHaveAttribute("title", lensEnd);
  } finally {
    await page.request.post(`${STUB}/__reset`);
  }
  gate.assertClean();
});

// shading#29 (live v0.2.12, PP cam6): "+" from f/3.2 showed f/3.5 (the next fNumberChoices menu
// stop) and then the camera's f/3.3, a forward-then-back flicker. The stub moves `apertureAv` by
// the lens's fine step (0.1 AV) per `apertureStep`, like the relay: the guess shown at once must be
// the value the camera then confirms, and nothing else may ever show in between.
test("the aperture guess equals the confirmed value, never forward-then-back, console clean (shading#29)", async ({ page }) => {
  const gate = consoleGate(page);
  try {
    await open(page);
    const val = page.locator('[data-role="fnum"]');
    const inc = page.locator('[data-role="aperture-inc"]');
    const bar = page.locator('[data-role="aperture-pending"]');
    await expect(val).toHaveText("f/5.2");
    await page.request.post(`${STUB}/__write`, { data: { delayMs: 600 } });
    await startFnumSampler(page);
    const before = await writes(page);

    await inc.click();
    // At once: the guess (the write answers 600 ms later).
    await expect(val).toHaveText("f/5.4", { timeout: 300 });
    await expect(bar).toBeVisible();
    await expect(bar).toBeHidden({ timeout: 5000 });
    await expect.poll(() => writes(page)).toBe(before + 1);
    // Past the confirmation push: the camera's landing (AV 4.88) is the guess.
    await page.waitForTimeout(2500);
    await expect(val).toHaveText("f/5.4");

    // A second press builds on the camera's f-stop (the stub's 0.1 AV = the default step).
    await inc.click();
    await expect(val).toHaveText("f/5.6", { timeout: 300 });
    await expect(bar).toBeVisible();
    await expect(bar).toBeHidden({ timeout: 5000 });
    await page.waitForTimeout(2500);
    await expect(val).toHaveText("f/5.6");

    const seen = await stopFnumSampler(page);
    expect(seen, "only the start and each landing, no other value in between").toEqual([
      "f/5.2",
      "f/5.4",
      "f/5.6",
    ]);
    const state = await (await page.request.get(`${STUB}/api/state`)).json();
    expect(state.params.apertureAv).toBeCloseTo(4.98, 6);
    expect(await writes(page)).toBe(before + 2);
  } finally {
    await page.request.post(`${STUB}/__reset`);
  }
  gate.assertClean();
});

// The guess learns this camera's own step: a lens that moves 0.15 AV per step lands the first
// press past the default 0.1 AV guess, and from then on the guess is the landing.
test("the aperture guess learns the camera's own step from the last confirmed press, console clean (shading#29)", async ({ page }) => {
  const gate = consoleGate(page);
  try {
    await open(page);
    const val = page.locator('[data-role="fnum"]');
    const inc = page.locator('[data-role="aperture-inc"]');
    const bar = page.locator('[data-role="aperture-pending"]');
    await expect(val).toHaveText("f/5.2");
    await page.request.post(`${STUB}/__write`, { data: { delayMs: 600, stepAv: 0.15 } });

    // Nothing learned yet: the default 0.1 AV (f/5.4); the lens lands at AV 4.93 = f/5.5.
    await inc.click();
    await expect(val).toHaveText("f/5.4", { timeout: 300 });
    await expect(bar).toBeVisible();
    await expect(bar).toBeHidden({ timeout: 5000 });
    await expect(val).toHaveText("f/5.5", { timeout: 3000 });
    await page.waitForTimeout(2500);

    // Learned 0.15 AV: the guess is the next landing, AV 5.08 = f/5.8 (the default would say f/5.7).
    await startFnumSampler(page);
    await inc.click();
    await expect(val).toHaveText("f/5.8", { timeout: 300 });
    await expect(bar).toBeVisible();
    await expect(bar).toBeHidden({ timeout: 5000 });
    await page.waitForTimeout(2500);
    await expect(val).toHaveText("f/5.8");
    const seen = await stopFnumSampler(page);
    expect(seen).toEqual(["f/5.5", "f/5.8"]);
    const state = await (await page.request.get(`${STUB}/api/state`)).json();
    expect(state.params.apertureAv).toBeCloseTo(5.08, 6);
  } finally {
    await page.request.post(`${STUB}/__reset`);
  }
  gate.assertClean();
});
