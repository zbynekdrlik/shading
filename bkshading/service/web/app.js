"use strict";
// bkshading web panel (issue 808). Server-truth with an OPTIMISTIC echo (issue 1337): a control
// shows its new value immediately as "pending" the instant it is clicked, then the next server push
// (from /ws, or the /api/cameras poll fallback) RECONCILES it — the server stays the single source
// of truth, the optimistic value just removes the click->confirm lag the owner reported. Controls
// PUT a shading change to /api/cameras/<id>/params (forwarded to the camera's relay). M2: a camera with an NDI
// preview shows a live JPEG preview (top block) reloaded a few times a second from
// /api/cameras/<id>/preview.jpg; a camera with no preview shows a params-only block.

const grid = document.getElementById("camera-grid");
const tmpl = document.getElementById("camera-block");
const connEl = document.getElementById("conn-status");
const emptyNote = document.getElementById("empty-note");
const blocks = new Map(); // camera id -> block element (reused to preserve control focus)
let interacting = false; // pause re-render while the operator is dragging a control

// Live preview refresh rate (Hz). Shading is about colour/exposure, not motion, so a few
// fps is plenty; keep it in step with the service-side decimation (~3 fps).
const PREVIEW_FPS = 3;

// Present f-number from the AV the relay reported: fNumber = sqrt(2^AV).
function fNumberFromAv(av) {
  return av == null ? null : Math.sqrt(Math.pow(2, av));
}

async function setParam(id, patch) {
  try {
    await fetch(`/api/cameras/${encodeURIComponent(id)}/params`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(patch),
    });
  } catch (e) {
    // The write failed; the optimistic pending value is reconciled (reverted) by the next server
    // push (server-truth). Log for diagnosis.
    console.warn("set failed", id, e);
  }
}

// issue 1304: +/- step amounts. Aperture steps by ONE camera f-number choice (computed from
// caps.fNumberChoices); the white balance and tint step by a fixed operator-sized amount.
const KELVIN_STEP = 100; // K per tap
const TINT_STEP = 1; // tint units per tap

// issue 1304: a block's f-number choices, stored on its dataset by updateBlock (from
// caps.fNumberChoices). Empty/absent -> the aperture +/- step is disabled (never fabricated).
function readFnumChoices(el) {
  try {
    const a = JSON.parse(el.dataset.fnumberChoices || "[]");
    return Array.isArray(a) ? a : [];
  } catch (e) {
    return [];
  }
}

// issue 1304: enable/disable the six +/- step buttons. Aperture is disabled entirely when the
// relay sends no f-number choices (a title explains why — never a fabricated step) and at the
// choice-index bounds; kelvin/tint are disabled at the slider's own min/max. Called by
// updateBlock (each poll) and after each step so a tap that reaches a bound disables promptly.
function refreshStepDisabled(el) {
  const q = (role) => el.querySelector(`[data-role="${role}"]`);
  const choices = readFnumChoices(el);
  const apDec = q("aperture-dec");
  const apInc = q("aperture-inc");
  if (choices.length < 2) {
    for (const b of [apDec, apInc]) {
      b.disabled = true;
      b.title = "Kroky clony nie sú dostupné (relay neposiela voľby clony)";
    }
  } else {
    // issue 1337: bounds from the REAL current f-number, not the slider norm. On-grid at an extreme
    // disables that direction; off-grid (open below/above the grid, or between two stops) leaves
    // BOTH enabled so the first step moves the lens onto the grid (the owner's "-" must work).
    const n = choices.length;
    const cur = currentFnum(el);
    const onGridIdx = Number.isFinite(cur)
      ? choices.findIndex((c) => Math.abs(c - cur) < 1e-6)
      : -1;
    apDec.disabled = onGridIdx === 0;
    apInc.disabled = onGridIdx === n - 1;
    apDec.title = "";
    apInc.title = "";
  }
  for (const [role, dec, inc] of [
    ["kelvin", "kelvin-dec", "kelvin-inc"],
    ["tint", "tint-dec", "tint-inc"],
  ]) {
    const s = q(role);
    const cur = Number(s.value);
    q(dec).disabled = cur <= Number(s.min);
    q(inc).disabled = cur >= Number(s.max);
  }
}

// issue 1337: JS mirror of the proto `mapping::step_choice` — the target choice INDEX stepping the
// aperture from the camera's REAL current f-number across the enumerated grid. "+" = the first
// choice strictly ABOVE the current f-number, "-" = the last strictly BELOW; a lens open below the
// first enumerated stop clamps to the min choice from either direction (so the FIRST step moves it
// ONTO the grid, never the pre-fix nearest-snap-to-idx-0 +1 that owner reported as "clona sa
// nezdvihne"). Ascending choices. Pinned against the Rust spec by test_app_js_step_choice_1337.
function stepChoice(currentFnum, choices, dir) {
  const n = choices.length;
  if (n === 0) return null;
  if (n === 1) return 0;
  if (dir === 0) return 0; // parity with the Rust step_choice (dir 0 is never sent by the panel)
  if (dir > 0) {
    const i = choices.findIndex((c) => c > currentFnum);
    return i === -1 ? n - 1 : i;
  }
  for (let i = n - 1; i >= 0; i--) if (choices[i] < currentFnum) return i;
  return 0;
}

// issue 1337: the block's REAL current f-number, or NaN when aperture is unknown. Guards the
// `Number("") === 0` trap — an unreadable aperture (dataset "") must NOT read as a finite 0.
function currentFnum(el) {
  const raw = el.dataset.apertureFnum;
  return raw === "" || raw == null ? NaN : Number(raw);
}

// issue 1304 + 1337: step the aperture by ONE f-number choice, from the camera's REAL current
// f-number (stored on the dataset from apertureAv) — NOT from the slider's normalised position,
// which an off-grid lens reports as 0 (the #1337 bug). The target index comes from stepChoice; the
// panel sets the slider locally + shows the target f-number as an OPTIMISTIC pending confirmation
// (reconciled by the next server push), and PUTs the ABSOLUTE apertureNorm = idx/(n-1) — the exact
// inverse of the relay's norm_to_choice_index over the SAME choice list. One tap = one PUT.
function stepAperture(el, id, dir) {
  const choices = readFnumChoices(el);
  if (choices.length < 2) return; // no choices -> no fabricated step (the button is disabled)
  const n = choices.length;
  const s = el.querySelector('[data-role="aperture"]');
  const curFnum = currentFnum(el);
  const idx = Number.isFinite(curFnum)
    ? stepChoice(curFnum, choices, dir)
    : Math.min(n - 1, Math.max(0, Math.round(Number(s.value) * (n - 1)) + dir));
  const norm = idx / (n - 1);
  s.value = norm;
  // Optimistic: show the target f-number immediately (pending) so the number moves on the first tap.
  const fnEl = el.querySelector('[data-role="fnum"]');
  fnEl.textContent = "f/" + choices[idx].toFixed(1);
  fnEl.classList.add("pending");
  el.dataset.apertureFnum = String(choices[idx]);
  setParam(id, { apertureNorm: norm });
  refreshStepDisabled(el);
}

// issue 1304: step a linear slider (kelvin by KELVIN_STEP K, tint by TINT_STEP) by `amount`,
// clamped to the slider's own min/max, sending the ABSOLUTE new value. One tap = one PUT, no
// auto-repeat on hold (the issue-1229 USB-PTP bus doctrine: one write = one gphoto2 session).
function stepLinear(el, id, role, key, amount, dir) {
  const s = el.querySelector(`[data-role="${role}"]`);
  const min = Number(s.min);
  const max = Number(s.max);
  let next = Math.round(Number(s.value)) + dir * amount;
  next = Math.min(max, Math.max(min, next));
  s.value = next;
  // issue 1337: optimistic pending label so the number moves on the first tap (reconciled by the
  // next server push).
  const valEl = el.querySelector(`[data-role="${role}-val"]`);
  if (valEl) {
    valEl.textContent = role === "kelvin" ? next + "K" : String(next);
    valEl.classList.add("pending");
  }
  setParam(id, { [key]: next });
  refreshStepDisabled(el);
}

// Wire a freshly cloned block's controls to their PUT handlers (attached once per block).
function wire(el, id) {
  const q = (role) => el.querySelector(`[data-role="${role}"]`);
  // Pause the 2s re-render for the whole block while a control is being touched, so a poll
  // between a button's pointerdown and its click never rebuilds (and eats) the tap.
  el.addEventListener("pointerdown", () => (interacting = true));
  el.addEventListener("pointerup", () => setTimeout(() => (interacting = false), 250));
  const guard = (fn) => (ev) => {
    interacting = false;
    fn(ev);
  };
  ["aperture", "kelvin", "tint"].forEach((role) => {
    const input = q(role);
    input.addEventListener("pointerdown", () => (interacting = true));
    input.addEventListener("focus", () => (interacting = true));
    input.addEventListener("blur", () => (interacting = false));
  });
  // issue 1337: each slider change echoes an OPTIMISTIC pending value immediately (reconciled by
  // the next server push) so the number moves the instant the operator releases the slider.
  q("aperture").addEventListener("change", guard((e) => {
    const norm = Number(e.target.value);
    const choices = readFnumChoices(el);
    if (choices.length >= 2) {
      const idx = Math.min(choices.length - 1, Math.max(0, Math.round(norm * (choices.length - 1))));
      q("fnum").textContent = "f/" + choices[idx].toFixed(1);
      q("fnum").classList.add("pending");
      el.dataset.apertureFnum = String(choices[idx]);
    }
    setParam(id, { apertureNorm: norm });
  }));
  q("kelvin").addEventListener("change", guard((e) => {
    const v = Math.round(Number(e.target.value));
    q("kelvin-val").textContent = v + "K";
    q("kelvin-val").classList.add("pending");
    setParam(id, { kelvin: v });
  }));
  q("tint").addEventListener("change", guard((e) => {
    const v = Math.round(Number(e.target.value));
    q("tint-val").textContent = String(v);
    q("tint-val").classList.add("pending");
    setParam(id, { tint: v });
  }));
  q("auto-wb").addEventListener("click", () => setParam(id, { autoWb: true }));

  // issue 1304: +/- step buttons. Each is a plain CLICK handler (one tap = one PUT) — NEVER a
  // pointerdown-hold with a repeat timer, per the issue-1229 USB-PTP bus doctrine. `guard`
  // clears `interacting` like the slider's change handler so the next render isn't eaten.
  q("aperture-dec").addEventListener("click", guard(() => stepAperture(el, id, -1)));
  q("aperture-inc").addEventListener("click", guard(() => stepAperture(el, id, 1)));
  q("kelvin-dec").addEventListener("click", guard(() => stepLinear(el, id, "kelvin", "kelvin", KELVIN_STEP, -1)));
  q("kelvin-inc").addEventListener("click", guard(() => stepLinear(el, id, "kelvin", "kelvin", KELVIN_STEP, 1)));
  q("tint-dec").addEventListener("click", guard(() => stepLinear(el, id, "tint", "tint", TINT_STEP, -1)));
  q("tint-inc").addEventListener("click", guard(() => stepLinear(el, id, "tint", "tint", TINT_STEP, 1)));

  // issue 809: explicit "align camera fps to the box's grab mode" button. Never an auto-write
  // (a camera-side format change can interrupt recording) — the operator must click. The grab
  // target is read from the block's dataset (kept current by updateBlock), so the handler is
  // wired once and always sends the latest configured grab fps.
  const setGrabBtn = q("fps-set-grab");
  if (setGrabBtn) {
    setGrabBtn.addEventListener("click", () => {
      const g = Number(el.dataset.grabFps);
      if (Number.isFinite(g) && g > 0) setParam(id, { fps: g });
    });
  }

  // Preview image: show it once a frame loads, fall back to the placeholder on error (503
  // until the first frame, or a dropped feed). Wired once per block.
  const img = q("preview-img");
  const ph = q("preview-placeholder");
  if (img) {
    img.addEventListener("load", () => {
      img.classList.add("ready");
      if (ph) ph.hidden = true;
    });
    img.addEventListener("error", () => {
      img.classList.remove("ready");
      if (ph) ph.hidden = false;
      el.dataset.previewErrAt = String(Date.now());
    });
  }
}

// Reload each preview-capable block's <img> from the service. Cache-busting query so the
// browser fetches a fresh frame; a 503/404 fires the img's error handler (placeholder shown).
function refreshPreviews() {
  const now = Date.now();
  for (const [id, el] of blocks) {
    if (el.dataset.preview !== "1") continue;
    // Brief backoff after a failed load, so a down camera (503/404) isn't hit at the full rate.
    if (now - Number(el.dataset.previewErrAt || 0) < 1500) continue;
    const img = el.querySelector('[data-role="preview-img"]');
    if (img) img.src = `/api/cameras/${encodeURIComponent(id)}/preview.jpg?t=${now}`;
  }
}

// Rebuild a value-button group (ISO, shutter) from the camera caps, marking the current one.
function renderButtonGroup(container, values, current, onPick) {
  container.textContent = "";
  for (const v of values) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "btn val-btn" + (v === current ? " active" : "");
    b.textContent = String(v);
    b.addEventListener("click", () => onPick(v));
    container.appendChild(b);
  }
}

function updateBlock(el, cam) {
  const q = (role) => el.querySelector(`[data-role="${role}"]`);
  q("label").textContent = cam.label;

  // Preview area: shown only for cameras that carry an NDI preview. The `preview` dataset
  // flag drives refreshPreviews() (which reloads the <img>); a camera without a feed keeps
  // its preview area hidden and gets no image reloads.
  const preview = q("preview");
  if (preview) preview.hidden = !cam.hasPreview;
  el.dataset.preview = cam.hasPreview ? "1" : "0";

  const online = cam.reachable && cam.state && cam.state.online;
  q("online").textContent = !cam.reachable ? "relay offline" : online ? "online" : "kamera offline";
  q("online").className = "cam-online " + (online ? "ok" : "bad");
  el.classList.toggle("disabled", !online);

  const p = online ? cam.state.params : {};
  const caps = online && cam.state.caps ? cam.state.caps : null;

  // Aperture. issue 1337: store the REAL current f-number on the dataset (stepAperture/
  // refreshStepDisabled step from it, not the off-grid slider norm) and reconcile any optimistic
  // pending value from a step with this authoritative push.
  const fn = fNumberFromAv(p.apertureAv);
  const fnumEl = q("fnum");
  fnumEl.textContent = fn == null ? "f/—" : "f/" + fn.toFixed(1);
  fnumEl.classList.remove("pending");
  el.dataset.apertureFnum = fn == null ? "" : String(fn);
  const apEl = q("aperture");
  if (document.activeElement !== apEl && p.apertureNorm != null) apEl.value = p.apertureNorm;

  // ISO. Skip the button REBUILD while interacting so a poll can't replace a button mid-tap; the
  // value label + optimistic pending still reconcile live (issue 1337).
  const isoVal = q("iso-val");
  isoVal.textContent = p.iso == null ? "—" : String(p.iso);
  isoVal.classList.remove("pending");
  if (!interacting) {
    renderButtonGroup(q("iso"), caps ? caps.isoChoices : [], p.iso, (v) => {
      isoVal.textContent = String(v); // optimistic
      isoVal.classList.add("pending");
      setParam(cam.id, { iso: v });
    });
  }

  // White balance.
  const kVal = q("kelvin-val");
  kVal.textContent = p.kelvin == null ? "—" : p.kelvin + "K";
  kVal.classList.remove("pending");
  const kEl = q("kelvin");
  if (document.activeElement !== kEl && p.kelvin != null) kEl.value = p.kelvin;
  const tVal = q("tint-val");
  tVal.textContent = p.tint == null ? "—" : String(p.tint);
  tVal.classList.remove("pending");
  const tEl = q("tint");
  if (document.activeElement !== tEl && p.tint != null) tEl.value = p.tint;

  // issue 1304: expose the camera's f-number choices for the aperture +/- step (stored on the
  // dataset, read by the step handler — the same pattern as grabFps), and refresh the
  // enable/disable state of all six step buttons (disabled without choices and at the bounds).
  const fnumChoices = caps && Array.isArray(caps.fNumberChoices) ? caps.fNumberChoices : [];
  el.dataset.fnumberChoices = JSON.stringify(fnumChoices);
  refreshStepDisabled(el);

  // Shutter. Same interacting-guard + optimistic pending as ISO (issue 1337).
  const shVal = q("shutter-val");
  shVal.textContent = p.shutter == null ? "—" : "1/" + p.shutter;
  shVal.classList.remove("pending");
  if (!interacting) {
    renderButtonGroup(q("shutter"), caps ? caps.shutterChoices : [], p.shutter, (v) => {
      shVal.textContent = "1/" + v; // optimistic
      shVal.classList.add("pending");
      setParam(cam.id, { shutter: v });
    });
  }

  // fps + issue-809 grab-mode sync.
  const camFps = p.fps100 == null ? null : p.fps100 / 100;
  q("fps-val").textContent = camFps == null ? "—" : camFps.toFixed(2);
  // Effective grab fps (issue 809): the box's live capture rate when the relay reports one,
  // else the static config; null => no comparison for this camera.
  const grab = cam.grabFps;
  el.dataset.grabFps = grab == null ? "" : String(grab);
  const syncRow = q("fps-sync");
  const grabEl = q("fps-grab");
  const desyncEl = q("fps-desync");
  const warnEl = q("fps-warn");
  const setBtn = q("fps-set-grab");
  // Hide the whole sync row (not just its children) for a camera with no grab configured,
  // so no empty gap shows under the "fps —" line (a handheld without a grab mode).
  if (syncRow) syncRow.hidden = grab == null;
  if (grab == null) {
    grabEl.hidden = true;
    if (desyncEl) desyncEl.hidden = true;
    warnEl.hidden = true;
    setBtn.hidden = true;
  } else {
    grabEl.textContent = "grab " + grab;
    grabEl.hidden = false;
    // issue 809: the static config grab_fps disagrees with the box's live capture rate — the
    // panel compares against the live rate (grab above) and flags the stale config.
    if (desyncEl) {
      desyncEl.hidden = !cam.grabFpsDesync;
      if (cam.grabFpsDesync) desyncEl.textContent = `⚠ config ≠ box capture (${grab})`;
    }
    const mismatch = cam.fpsSync === "mismatch";
    warnEl.hidden = !mismatch;
    if (mismatch) {
      warnEl.textContent = `⚠ kamera ${camFps == null ? "?" : camFps.toFixed(2)} ≠ grab ${grab}`;
    }
    // The align button appears only when there is a mismatch to fix AND it is actionable
    // (camera online and its project fps is settable). Server-truth: after the write the next
    // poll re-reads the camera and the warning/button clear on their own.
    const settable = online && cam.state && cam.state.fpsSupported;
    setBtn.hidden = !(mismatch && settable);
    setBtn.textContent = `Zosúladiť s grab (${grab})`;
  }
}

function render(agg) {
  document.getElementById("app-version").textContent = "v" + agg.version;
  // issue 1337: DO NOT drop the whole push while interacting — that made every confirmation
  // arriving during a click sequence invisible (the owner's 2-3 s number lag). updateBlock protects
  // only the control being actively dragged (activeElement) and the ISO/shutter button REBUILD
  // (which could eat a mid-tap); every value LABEL still reconciles live.
  emptyNote.hidden = agg.cameras.length !== 0;
  const seen = new Set();
  for (const cam of agg.cameras) {
    seen.add(cam.id);
    let el = blocks.get(cam.id);
    if (!el) {
      el = tmpl.content.firstElementChild.cloneNode(true);
      el.dataset.id = cam.id;
      wire(el, cam.id);
      grid.appendChild(el);
      blocks.set(cam.id, el);
    }
    updateBlock(el, cam);
  }
  for (const [id, el] of blocks) {
    if (!seen.has(id)) {
      el.remove();
      blocks.delete(id);
    }
  }
}

async function poll() {
  try {
    const r = await fetch("/api/cameras", { cache: "no-store" });
    if (!r.ok) throw new Error("HTTP " + r.status);
    connEl.textContent = "online";
    connEl.classList.remove("bad");
    render(await r.json());
  } catch (e) {
    connEl.textContent = "offline";
    connEl.classList.add("bad");
  }
}

// issue 808 — live state push over WebSocket (server = single source of truth): the service
// pushes the whole aggregate on connect and on every change as {"type":"state", version,
// cameras} (the flattened dev2-MVP envelope), so render() consumes it directly. HTTP /api/cameras
// polling stays as a FALLBACK, active only while the WS is down (an old browser, a proxy that
// blocks WS). Writes still go over HTTP PUT (setParam) — the WS is push-only.
let ws = null;
let wsConnected = false;
let wsBackoff = 1000; // reconnect backoff (ms), doubling up to a cap.

function wsUrl() {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${location.host}/ws`;
}

function connectWs() {
  let sock;
  try {
    sock = new WebSocket(wsUrl());
  } catch (e) {
    scheduleWsReconnect();
    return;
  }
  ws = sock;
  sock.addEventListener("open", () => {
    wsConnected = true;
    wsBackoff = 1000;
    connEl.textContent = "online";
    connEl.classList.remove("bad");
  });
  sock.addEventListener("message", (ev) => {
    try {
      const msg = JSON.parse(ev.data);
      // Flattened envelope: {type:"state", version, cameras} — render() reads version/cameras.
      if (msg && msg.type === "state") render(msg);
    } catch (e) {
      console.warn("bad ws message", e);
    }
  });
  sock.addEventListener("close", () => {
    wsConnected = false;
    scheduleWsReconnect();
  });
  // An error is always followed by close; close drives the reconnect, so just log here.
  sock.addEventListener("error", () => console.warn("ws error"));
}

function scheduleWsReconnect() {
  connEl.textContent = "offline";
  connEl.classList.add("bad");
  setTimeout(connectWs, wsBackoff);
  wsBackoff = Math.min(wsBackoff * 2, 15000);
}

poll(); // immediate first paint over HTTP, before the WS handshake completes
connectWs(); // primary live channel
// HTTP poll is the FALLBACK — it runs only while the WS is not connected.
setInterval(() => {
  if (!wsConnected) poll();
}, 2000);
setInterval(refreshPreviews, Math.round(1000 / PREVIEW_FPS));

// issue 1305: register the service worker so the panel is installable as a PWA (own icon,
// standalone window in the Windows dock). The SW is a pure network passthrough (no cache —
// server-truth). Guarded on the API being present: on an insecure-origin LAN page (plain http
// on a bare hostname) `navigator.serviceWorker` is undefined, so this is a no-op and the console
// stays clean; the `.catch` swallows any registration error so nothing is logged either way.
if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("/sw.js").catch(() => {});
}
