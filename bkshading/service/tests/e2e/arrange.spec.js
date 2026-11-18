// @ts-check
// Real-browser E2E of ARRANGE in the rebuilt panel (shading#16, owner-approved mockup arrange-v1):
// the ⇄ key, dragging blocks (swap on a block's middle, move at its edge), the eye (hide / show),
// the ↺ reset, the save for the whole venue (a second page gets the push), the refusals, the end on
// a page hide, and controls inert under the covers. Against a real service with THREE cameras on
// the stub relay (e2e-arrange-config.toml, config order 3 1 2). Every test ends on a clean console
// and puts the venue layout back to number order (one shared service).
const { test, expect } = require("@playwright/test");
const { PANEL: PANEL_PATH, consoleGate, neutraliseServiceWorker, DISABLE_WS, waitForApp, recorded } = require("./next-helpers");

const SVC = process.env.ARRANGE_SVC_BASE_URL || "http://127.0.0.1:8784";
const PANEL = `${SVC}${PANEL_PATH}`;

test.beforeEach(async ({ page }) => {
  await neutraliseServiceWorker(page);
  // shading#24: a card is now up to 460 px wide and ~900 px tall; the drags grab a card's middle,
  // which must lie inside the window.
  await page.setViewportSize({ width: 1920, height: 1200 });
});

/** The venue layout back to number order, nothing hidden (after every test). */
async function resetLayout(request) {
  const doc = await (await request.get(`${SVC}/api/layout`)).json();
  const r = await request.put(`${SVC}/api/layout`, { data: { rev: doc.rev, order: [], hidden: [] } });
  expect(r.status()).toBe(200);
}

test.afterEach(async ({ request }) => {
  await resetLayout(request);
});

const order = (page) => page.locator("#camera-grid > [data-id]").evaluateAll((els) => els.map((e) => e.dataset.id));
const block = (page, id) => page.locator(`#camera-grid > [data-id="${id}"]`);
const key = (page) => page.locator('[data-role="arrange-key"]');
const note = (page) => page.locator('[data-role="arrange-note"]');

/** Opens the panel and waits for its three blocks. */
async function open(page) {
  await page.goto(PANEL);
  await waitForApp(page);
  await expect.poll(() => order(page), { timeout: 15000 }).toEqual(["cam1", "cam2", "cam3"]);
}

/** Drags block `from` to a point of block `to`: `at` 0.5 = the middle (swap), 0.08 / 0.92 = edges. */
async function drag(page, from, to, at) {
  const a = await block(page, from).boundingBox();
  const b = await block(page, to).boundingBox();
  await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2);
  await page.mouse.down();
  await page.mouse.move(a.x + a.width / 2 + 10, a.y + a.height / 2 + 4, { steps: 3 });
  await page.mouse.move(b.x + b.width * at, b.y + b.height / 2, { steps: 8 });
  await page.mouse.up();
}

test("ARRANGE: swap and move by drag, save for the venue, a second device follows, console clean", async ({ page, browser }) => {
  const gate = consoleGate(page);
  await open(page);
  // A second device at the venue, open the whole time.
  const other = await browser.newPage();
  const otherGate = consoleGate(other);
  await neutraliseServiceWorker(other);
  await open(other);

  await key(page).click();
  await expect(key(page)).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".app")).toHaveAttribute("data-arrange", "true");
  await expect(note(page)).toHaveText("ťahaj ⠿ · oko skryje/ukáže · ⇄ = uložiť");
  await expect(block(page, "cam1").locator('[data-role="cover"]')).toBeVisible();

  // cam3 onto cam1's middle: they swap.
  await drag(page, "cam3", "cam1", 0.5);
  await expect.poll(() => order(page)).toEqual(["cam3", "cam2", "cam1"]);
  // cam1 onto cam3's left edge: it moves before it.
  await drag(page, "cam1", "cam3", 0.06);
  await expect.poll(() => order(page)).toEqual(["cam1", "cam3", "cam2"]);
  // The other device still shows the saved layout, not the edit.
  expect(await order(other)).toEqual(["cam1", "cam2", "cam3"]);

  await key(page).click();
  await expect(note(page)).toHaveText("Uložené pre všetky zariadenia");
  await expect(note(page)).toHaveClass(/saved/);
  await expect(page.locator(".app")).toHaveAttribute("data-arrange", "false");
  await expect.poll(() => order(page)).toEqual(["cam1", "cam3", "cam2"]);

  // The service stored it, and pushed it to the other device without a reload.
  const doc = await (await page.request.get(`${SVC}/api/layout`)).json();
  expect(doc.order).toEqual(["cam1", "cam3", "cam2"]);
  expect(doc.hidden).toEqual([]);
  await expect.poll(() => order(other), { timeout: 5000 }).toEqual(["cam1", "cam3", "cam2"]);

  // A reload keeps it.
  await page.reload();
  await waitForApp(page);
  await expect.poll(() => order(page), { timeout: 15000 }).toEqual(["cam1", "cam3", "cam2"]);
  otherGate.assertClean();
  await other.close();
  gate.assertClean();
});

test("ARRANGE: the eye hides a camera from the panel, the chip leads back, the eye shows it again, console clean", async ({ page }) => {
  const gate = consoleGate(page);
  await open(page);
  const chip = page.locator('[data-role="hidden-chip"]');
  await expect(chip).toBeHidden();

  await key(page).click();
  await block(page, "cam2").locator('[data-role="eye"]').click();
  await expect(block(page, "cam2")).toHaveClass(/ghost/);
  await expect(block(page, "cam2").locator('[data-role="eye"]')).toHaveAttribute("aria-pressed", "true");
  // In ARRANGE the hidden camera keeps its place, dashed.
  expect(await order(page)).toEqual(["cam1", "cam2", "cam3"]);
  await key(page).click();
  await expect(note(page)).toHaveText("Uložené pre všetky zariadenia");

  // Outside ARRANGE the grid closes up; the header says how to get it back.
  await expect.poll(() => order(page)).toEqual(["cam1", "cam3"]);
  await expect(chip).toBeVisible();
  await expect(chip).toHaveText("1 skrytá kamera · vrátiš ju cez ⇄");
  expect((await (await page.request.get(`${SVC}/api/layout`)).json()).hidden).toEqual(["cam2"]);
  // Hiding is panel-only: the hidden camera is still polled by the service.
  const cams = await (await page.request.get(`${SVC}/api/cameras`)).json();
  expect(cams.cameras.find((c) => c.id === "cam2").state.online).toBe(true);

  // The chip starts ARRANGE; the eye brings the camera back.
  await chip.click();
  await expect(page.locator(".app")).toHaveAttribute("data-arrange", "true");
  await expect(block(page, "cam2")).toHaveClass(/ghost/);
  await block(page, "cam2").locator('[data-role="eye"]').click();
  await expect(block(page, "cam2")).not.toHaveClass(/ghost/);
  await key(page).click();
  await expect.poll(() => order(page)).toEqual(["cam1", "cam2", "cam3"]);
  await expect(chip).toBeHidden();
  gate.assertClean();
});

test("ARRANGE: ↺ podľa čísel restores number order with every camera shown, console clean", async ({ page }) => {
  const gate = consoleGate(page);
  // Start from a stored layout 3 2 1 with cam1 hidden.
  const doc = await (await page.request.get(`${SVC}/api/layout`)).json();
  const put = await page.request.put(`${SVC}/api/layout`, { data: { rev: doc.rev, order: ["cam3", "cam2", "cam1"], hidden: ["cam1"] } });
  expect(put.status()).toBe(200);
  await page.goto(PANEL);
  await waitForApp(page);
  await expect.poll(() => order(page), { timeout: 15000 }).toEqual(["cam3", "cam2"]);

  const reset = page.locator('[data-role="reset-num"]');
  await expect(reset).toBeHidden();
  await key(page).click();
  await expect(reset).toBeVisible();
  await reset.click();
  await expect.poll(() => order(page)).toEqual(["cam1", "cam2", "cam3"]);
  await expect(block(page, "cam1")).not.toHaveClass(/ghost/);
  // Nothing is saved until the second tap.
  expect((await (await page.request.get(`${SVC}/api/layout`)).json()).order).toEqual(["cam3", "cam2", "cam1"]);
  await key(page).click();
  await expect(note(page)).toHaveText("Uložené pre všetky zariadenia");
  const saved = await (await page.request.get(`${SVC}/api/layout`)).json();
  expect(saved.order).toEqual(["cam1", "cam2", "cam3"]);
  expect(saved.hidden).toEqual([]);
  gate.assertClean();
});

test("ARRANGE: two taps without a change save nothing, and every control is inert under the covers, console clean", async ({ page }) => {
  const gate = consoleGate(page);
  await open(page);
  const rev = (await (await page.request.get(`${SVC}/api/layout`)).json()).rev;
  const before = (await recorded(page)).length;

  await key(page).click();
  // A tap where cam1's kelvin + sits lands on the cover: no write reaches the camera.
  const plus = await block(page, "cam1").locator('[data-role="kelvin-inc"]').boundingBox();
  await page.mouse.click(plus.x + plus.width / 2, plus.y + plus.height / 2);
  await page.waitForTimeout(800);
  expect((await recorded(page)).length, "no camera write in ARRANGE").toBe(before);
  // A tap on the cover (no drag) moves nothing either.
  expect(await order(page)).toEqual(["cam1", "cam2", "cam3"]);

  await key(page).click();
  await expect(note(page)).toHaveText("Bez zmeny");
  await expect(page.locator(".app")).toHaveAttribute("data-arrange", "false");
  expect((await (await page.request.get(`${SVC}/api/layout`)).json()).rev, "no save").toBe(rev);
  // Back in the normal mode the controls work again.
  await block(page, "cam1").locator('[data-role="kelvin-inc"]').click();
  await expect.poll(async () => (await recorded(page)).length, { timeout: 10000 }).toBeGreaterThan(before);
  gate.assertClean();
});

test("ARRANGE: another device's save during the mode ends it unsaved, console clean", async ({ page }) => {
  const gate = consoleGate(page);
  await open(page);
  await key(page).click();
  await drag(page, "cam3", "cam1", 0.5);
  await expect.poll(() => order(page)).toEqual(["cam3", "cam2", "cam1"]);

  // Another device saves first (2 1 3).
  const doc = await (await page.request.get(`${SVC}/api/layout`)).json();
  const r = await page.request.put(`${SVC}/api/layout`, { data: { rev: doc.rev, order: ["cam2", "cam1", "cam3"], hidden: [] } });
  expect(r.status()).toBe(200);

  // The push ends this mode unsaved and shows the other device's layout.
  await expect(note(page)).toHaveText("Neuložené: poradie sa medzitým zmenilo", { timeout: 5000 });
  await expect(page.locator(".app")).toHaveAttribute("data-arrange", "false");
  await expect.poll(() => order(page)).toEqual(["cam2", "cam1", "cam3"]);
  gate.assertClean();
});

test("ARRANGE: a save the service refuses as stale shows the refusal, console clean", async ({ page }) => {
  const gate = consoleGate(page);
  // This page hears no push (no socket, its layout polls answer the old document), so it saves on a
  // revision another device already replaced: the service answers 200 {"error":"stale"} (shading#16:
  // a refusal is never a 4xx, which Chromium would log as a console error).
  await page.addInitScript(DISABLE_WS);
  const stale = await (await page.request.get(`${SVC}/api/layout`)).json();
  await page.route("**/api/layout", (route) => {
    if (route.request().method() === "GET") {
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(stale) });
    }
    return route.continue();
  });
  await open(page);
  await key(page).click();
  const r = await page.request.put(`${SVC}/api/layout`, { data: { rev: stale.rev, order: ["cam2", "cam1", "cam3"], hidden: [] } });
  expect(r.status()).toBe(200);

  await drag(page, "cam3", "cam1", 0.5);
  await key(page).click();
  await expect(note(page)).toHaveText("Neuložené: poradie sa medzitým zmenilo", { timeout: 5000 });
  await expect(note(page)).toHaveClass(/refused/);
  await expect(page.locator(".app")).toHaveAttribute("data-arrange", "false");
  // The other device's layout stands.
  expect((await (await page.request.get(`${SVC}/api/layout`)).json()).order).toEqual(["cam2", "cam1", "cam3"]);

  gate.assertClean();
});

test("ARRANGE: hiding the page ends the mode unsaved, console clean", async ({ page }) => {
  const gate = consoleGate(page);
  await open(page);
  await key(page).click();
  await drag(page, "cam3", "cam1", 0.5);
  await page.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { get: () => "hidden", configurable: true });
    // The real event bubbles (it is fired at the document with bubbles = true).
    document.dispatchEvent(new Event("visibilitychange", { bubbles: true }));
  });
  await expect(page.locator(".app")).toHaveAttribute("data-arrange", "false");
  await expect(note(page)).toHaveText("Neuložené");
  await expect.poll(() => order(page)).toEqual(["cam1", "cam2", "cam3"]);
  gate.assertClean();
});
