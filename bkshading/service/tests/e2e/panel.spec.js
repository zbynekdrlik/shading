// @ts-check
// Real-browser E2E for the issue-1304 panel +/- step buttons, extended in issue 1337 to the ISO and
// uzávierka steppers (e2e-real-user-testing.md): open the served panel, click + on clona / biely bod
// / tint / ISO / uzávierka, and assert the ABSOLUTE PUT bodies the service forwarded to the (stub)
// relay — plus a clean browser console (browser-console-zero-errors).
const { test, expect } = require("@playwright/test");

const STUB = process.env.STUB_BASE_URL || "http://127.0.0.1:8781";

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
