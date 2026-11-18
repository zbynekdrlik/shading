// @ts-check
// shading#27: a write's confirmation is never rolled back by a pump snapshot whose relay poll
// started before the write. Live on PP cam6 (v0.2.10) an aperture press showed f/3.8, flicked back
// to f/3.7 at ~920 ms and settled on f/3.8 only at the next pump tick.
//
// The service on :8787 polls its OWN stub relay (:8786, --apply-writes). The test delays that
// stub's GET /api/state by 1 s (the stub copies the state when the GET ARRIVES), waits until the
// service's pump poll is in flight, presses ISO +, and samples the shown value every 100 ms: once
// the new value shows, the old one must never come back.
const { test, expect } = require("@playwright/test");
const { consoleGate, neutraliseServiceWorker, waitForApp } = require("./next-helpers");

const FRESH_SVC = "http://127.0.0.1:8787";
const FRESH_STUB = "http://127.0.0.1:8786";

test.beforeEach(async ({ page }) => {
  await neutraliseServiceWorker(page);
});

async function gets(page) {
  return (await (await page.request.get(`${FRESH_STUB}/__gets`)).json()).gets;
}

test("a pump snapshot polled before a write never rolls the confirmed value back, console clean (shading#27)", async ({ page }) => {
  const gate = consoleGate(page);
  await page.request.post(`${FRESH_STUB}/__reset`);
  try {
    await page.goto(`${FRESH_SVC}/`);
    await waitForApp(page);
    const iso = page.locator('[data-role="iso-val"]');
    await expect(iso).toHaveText("400");
    await page.request.post(`${FRESH_STUB}/__lag`, { data: { stateDelayMs: 1000 } });

    // Wait for a pump poll to ARRIVE (it now answers the pre-write state 1 s later), then press.
    const before = await gets(page);
    await expect.poll(() => gets(page), { timeout: 6000, intervals: [50] }).toBeGreaterThan(before);
    await page.locator('[data-role="iso-inc"]').click();

    const samples = [];
    const start = Date.now();
    while (Date.now() - start < 4500) {
      samples.push(((await iso.textContent()) || "").trim());
      await page.waitForTimeout(100);
    }
    const first = samples.indexOf("800");
    expect(first, `the press shows 800: ${samples.join(" ")}`).toBeGreaterThanOrEqual(0);
    expect(samples.slice(first), `never back to 400 after 800: ${samples.join(" ")}`).not.toContain("400");
    expect(samples[samples.length - 1]).toBe("800");
  } finally {
    await page.request.post(`${FRESH_STUB}/__reset`);
  }
  gate.assertClean();
});
