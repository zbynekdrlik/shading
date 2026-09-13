// @ts-check
// Real-browser E2E for the issue-1304 panel +/- step buttons (e2e-real-user-testing.md): open
// the served panel, click + on clona / biely bod / tint, and assert the ABSOLUTE PUT bodies the
// service forwarded to the (stub) relay — plus a clean browser console (browser-console-zero-errors).
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

  // One tap each on clona / biely bod / tint.
  await apInc.click();
  await page.locator('[data-role="kelvin-inc"]').click();
  await page.locator('[data-role="tint-inc"]').click();

  // The service forwards each PUT to the stub relay; wait until all three land.
  await expect
    .poll(
      async () => (await (await page.request.get(`${STUB}/__recorded`)).json()).length,
      { timeout: 10000 }
    )
    .toBeGreaterThanOrEqual(3);

  const recorded = await (await page.request.get(`${STUB}/__recorded`)).json();
  // Aperture: server norm 2/3 -> choice index 2 (f/5.2) of 4; "+" steps to index 3 (f/8.0),
  // i.e. an absolute apertureNorm of 1.0.
  expect(recorded.some((b) => b.apertureNorm === 1), "aperture + -> apertureNorm 1.0").toBeTruthy();
  // Biely bod: 5600 + 100 K.
  expect(recorded.some((b) => b.kelvin === 5700), "kelvin + -> 5700").toBeTruthy();
  // Tint: 0 + 1.
  expect(recorded.some((b) => b.tint === 1), "tint + -> 1").toBeTruthy();

  expect(problems, `console problems: ${problems.join(" | ")}`).toEqual([]);
});
