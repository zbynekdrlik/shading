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
const connBanner = document.getElementById("conn-banner"); // issue 1343: loud offline banner
const emptyNote = document.getElementById("empty-note");
const blocks = new Map(); // camera id -> block element (reused to preserve control focus)

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

// A block's enumerated choice list, stored on its dataset by updateBlock. Empty/absent -> the
// matching +/- step is disabled (never fabricated). issue 1304 (aperture) + issue 1337 (ISO/shutter).
function readChoices(el, datasetKey) {
  try {
    const a = JSON.parse(el.dataset[datasetKey] || "[]");
    return Array.isArray(a) ? a : [];
  } catch (e) {
    return [];
  }
}

// issue 1304: the aperture's f-number choices (from caps.fNumberChoices).
function readFnumChoices(el) {
  return readChoices(el, "fnumberChoices");
}

// issue 1337: ISO and uzávierka share the aperture-style −/slider/+ stepper (owner: "selektory na
// iso ako samostatné tlačidlá je blbosť, daj to ako ostatné" + "uzávierka tiež"). Each is an
// enumerated choice list (caps.isoChoices / caps.shutterChoices); a tap steps ONE choice from the
// camera's REAL current value via the SHARED stepChoice, then PUTs the ABSOLUTE value (choices[idx])
// — not a norm. The slider is an INDEX (0..n-1) over the choices. One config per parameter is the
// single source of truth for its dataset keys, wire key, and value formatting.
const ISO_STEPPER = {
  role: "iso",
  valRole: "iso-val",
  choicesKey: "isoChoices",
  valKey: "isoVal",
  key: "iso",
  fmt: (v) => String(v),
  emptyTitle: "Kroky ISO nie sú dostupné (relay neposiela voľby ISO)",
};
const SHUTTER_STEPPER = {
  role: "shutter",
  valRole: "shutter-val",
  choicesKey: "shutterChoices",
  valKey: "shutterVal",
  key: "shutter",
  fmt: (v) => "1/" + v,
  emptyTitle: "Kroky uzávierky nie sú dostupné (relay neposiela voľby uzávierky)",
};
const ENUM_STEPPERS = [ISO_STEPPER, SHUTTER_STEPPER];

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
  // issue 1337: ISO/uzávierka bounds from the REAL current value (like the aperture), NEVER the
  // slider index. On-grid at an extreme disables that direction; off-grid (below/above/between the
  // grid) leaves BOTH enabled so the first step moves onto the grid. Disabled entirely when the
  // relay sends no choices (a title explains why — never a fabricated step).
  for (const cfg of ENUM_STEPPERS) {
    const dec = q(cfg.role + "-dec");
    const inc = q(cfg.role + "-inc");
    const choices = readChoices(el, cfg.choicesKey);
    if (choices.length < 2) {
      for (const b of [dec, inc]) {
        b.disabled = true;
        b.title = cfg.emptyTitle;
      }
    } else {
      const n = choices.length;
      const cur = currentEnum(el, cfg.valKey);
      const onGridIdx = Number.isFinite(cur) ? choices.findIndex((c) => c === cur) : -1;
      dec.disabled = onGridIdx === 0;
      inc.disabled = onGridIdx === n - 1;
      dec.title = "";
      inc.title = "";
    }
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

// issue 1337: a block's REAL current enumerated value (ISO/shutter), or NaN when unknown. Guards
// the `Number("") === 0` trap — an unreadable value (dataset "") must NOT read as a finite 0.
function currentEnum(el, valKey) {
  const raw = el.dataset[valKey];
  return raw === "" || raw == null ? NaN : Number(raw);
}

// issue 1337: index of the choice closest to `value` — positions the index slider from a server
// push, including an off-grid value the camera reports between/below the enumerated steps.
function nearestIndex(choices, value) {
  let best = 0;
  let bestDist = Infinity;
  for (let i = 0; i < choices.length; i++) {
    const d = Math.abs(choices[i] - value);
    if (d < bestDist) {
      bestDist = d;
      best = i;
    }
  }
  return best;
}

// issue 1337: step an enumerated parameter (ISO, uzávierka) by ONE choice from the camera's REAL
// current value across the grid, using the SAME stepChoice semantics as the aperture ("+" = the
// first choice above the current value, "-" = the last below; off-grid → onto the grid on the first
// tap). One tap = one PUT of the ABSOLUTE value; the panel sets the slider index locally and shows
// the target value as an OPTIMISTIC pending confirmation (reconciled by the next server push).
function stepEnum(el, id, cfg, dir) {
  const choices = readChoices(el, cfg.choicesKey);
  if (choices.length < 2) return; // no choices -> no fabricated step (the button is disabled)
  const n = choices.length;
  const s = el.querySelector(`[data-role="${cfg.role}"]`);
  const cur = currentEnum(el, cfg.valKey);
  const idx = Number.isFinite(cur)
    ? stepChoice(cur, choices, dir)
    : Math.min(n - 1, Math.max(0, Math.round(Number(s.value)) + dir));
  const value = choices[idx];
  s.value = idx;
  el.dataset[cfg.valKey] = String(value);
  const valEl = el.querySelector(`[data-role="${cfg.valRole}"]`);
  valEl.textContent = cfg.fmt(value);
  valEl.classList.add("pending");
  setParam(id, { [cfg.key]: value });
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
  // issue 1337: each slider change echoes an OPTIMISTIC pending value immediately (reconciled by
  // the next server push) so the number moves the instant the operator releases the slider. A slider
  // being dragged is protected from a mid-drag overwrite by the `document.activeElement` check in
  // updateBlock — never by dropping the whole push (that hid the confirmations, the owner's lag).
  q("aperture").addEventListener("change", (e) => {
    const norm = Number(e.target.value);
    const choices = readFnumChoices(el);
    if (choices.length >= 2) {
      const idx = Math.min(choices.length - 1, Math.max(0, Math.round(norm * (choices.length - 1))));
      q("fnum").textContent = "f/" + choices[idx].toFixed(1);
      q("fnum").classList.add("pending");
      el.dataset.apertureFnum = String(choices[idx]);
    }
    setParam(id, { apertureNorm: norm });
  });
  q("kelvin").addEventListener("change", (e) => {
    const v = Math.round(Number(e.target.value));
    q("kelvin-val").textContent = v + "K";
    q("kelvin-val").classList.add("pending");
    setParam(id, { kelvin: v });
  });
  q("tint").addEventListener("change", (e) => {
    const v = Math.round(Number(e.target.value));
    q("tint-val").textContent = String(v);
    q("tint-val").classList.add("pending");
    setParam(id, { tint: v });
  });
  // issue 1337: ISO/uzávierka index sliders — on release, map the index to the enumerated choice and
  // PUT the ABSOLUTE value, with the same optimistic pending echo (reconciled by the next push).
  for (const cfg of ENUM_STEPPERS) {
    q(cfg.role).addEventListener("change", (e) => {
      const choices = readChoices(el, cfg.choicesKey);
      if (!choices.length) return;
      const idx = Math.min(choices.length - 1, Math.max(0, Math.round(Number(e.target.value))));
      const value = choices[idx];
      el.dataset[cfg.valKey] = String(value);
      const valEl = q(cfg.valRole);
      valEl.textContent = cfg.fmt(value);
      valEl.classList.add("pending");
      setParam(id, { [cfg.key]: value });
    });
  }
  q("auto-wb").addEventListener("click", () => setParam(id, { autoWb: true }));

  // issue 1304 + 1337: +/- step buttons. Each is a plain CLICK handler (one tap = one PUT) — NEVER a
  // pointerdown-hold with a repeat timer, per the issue-1229 USB-PTP shared-bus doctrine (one write =
  // one gphoto2 session). Aperture is a norm step; ISO/uzávierka step the enumerated choice via
  // stepEnum; kelvin/tint are linear.
  q("aperture-dec").addEventListener("click", () => stepAperture(el, id, -1));
  q("aperture-inc").addEventListener("click", () => stepAperture(el, id, 1));
  q("kelvin-dec").addEventListener("click", () => stepLinear(el, id, "kelvin", "kelvin", KELVIN_STEP, -1));
  q("kelvin-inc").addEventListener("click", () => stepLinear(el, id, "kelvin", "kelvin", KELVIN_STEP, 1));
  q("tint-dec").addEventListener("click", () => stepLinear(el, id, "tint", "tint", TINT_STEP, -1));
  q("tint-inc").addEventListener("click", () => stepLinear(el, id, "tint", "tint", TINT_STEP, 1));
  q("iso-dec").addEventListener("click", () => stepEnum(el, id, ISO_STEPPER, -1));
  q("iso-inc").addEventListener("click", () => stepEnum(el, id, ISO_STEPPER, 1));
  q("shutter-dec").addEventListener("click", () => stepEnum(el, id, SHUTTER_STEPPER, -1));
  q("shutter-inc").addEventListener("click", () => stepEnum(el, id, SHUTTER_STEPPER, 1));

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

  // ISO. issue 1337: the aperture-style stepper — store the choices + REAL value on the dataset
  // (stepEnum/refreshStepDisabled step from them), position the index slider (guarded while the
  // operator drags it), and reconcile any optimistic pending value from a step.
  const isoVal = q("iso-val");
  isoVal.textContent = p.iso == null ? "—" : String(p.iso);
  isoVal.classList.remove("pending");
  const isoChoices = caps && Array.isArray(caps.isoChoices) ? caps.isoChoices : [];
  el.dataset.isoChoices = JSON.stringify(isoChoices);
  el.dataset.isoVal = p.iso == null ? "" : String(p.iso);
  const isoEl = q("iso");
  if (isoChoices.length >= 2) isoEl.max = isoChoices.length - 1;
  if (document.activeElement !== isoEl && p.iso != null && isoChoices.length) {
    isoEl.value = nearestIndex(isoChoices, p.iso);
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
  // dataset, read by the step handler — the same pattern as grabFps).
  const fnumChoices = caps && Array.isArray(caps.fNumberChoices) ? caps.fNumberChoices : [];
  el.dataset.fnumberChoices = JSON.stringify(fnumChoices);

  // Shutter. issue 1337: same aperture-style index-slider stepper as ISO.
  const shVal = q("shutter-val");
  shVal.textContent = p.shutter == null ? "—" : "1/" + p.shutter;
  shVal.classList.remove("pending");
  const shutterChoices = caps && Array.isArray(caps.shutterChoices) ? caps.shutterChoices : [];
  el.dataset.shutterChoices = JSON.stringify(shutterChoices);
  el.dataset.shutterVal = p.shutter == null ? "" : String(p.shutter);
  const shEl = q("shutter");
  if (shutterChoices.length >= 2) shEl.max = shutterChoices.length - 1;
  if (document.activeElement !== shEl && p.shutter != null && shutterChoices.length) {
    shEl.value = nearestIndex(shutterChoices, p.shutter);
  }

  // issue 1304 + 1337: refresh the enable/disable state of ALL step buttons (aperture, ISO,
  // uzávierka, kelvin, tint) now that every block's choices + real value are on the dataset.
  refreshStepDisabled(el);

  // issue 1343: WRITE-NOT-APPLIED surfacing. The relay compares each key a write-burst wrote against
  // the authoritative readback at burst close and sends `state.notApplied` (wire field names). A
  // value whose key is listed is rendered `.not-applied` (bad colour) with an explanatory title on
  // the value label AND its +/- stepper — so a silently-refused write (the BMPCC ACKs + ignores an
  // aperture/focus PTP write while ISO applies) is no longer invisible (today the optimistic value
  // just reverts). Runs AFTER refreshStepDisabled so it doesn't fight the disabled-reason titles.
  const notApplied =
    online && cam.state && Array.isArray(cam.state.notApplied) ? cam.state.notApplied : [];
  const NA_TITLE = "Kamera tento zápis neprijala";
  const flagNotApplied = (valRole, key, stepperRoles) => {
    const flagged = notApplied.includes(key);
    const valEl = q(valRole);
    if (valEl) {
      valEl.classList.toggle("not-applied", flagged);
      valEl.title = flagged ? NA_TITLE : "";
    }
    for (const r of stepperRoles) {
      const b = q(r);
      if (!b) continue;
      b.classList.toggle("not-applied", flagged);
      // Only touch the button title when we own it: set NA when flagged + enabled; clear only if it
      // is still our NA title (never clobber a disabled-reason title set by refreshStepDisabled).
      if (flagged && !b.disabled) b.title = NA_TITLE;
      else if (!flagged && b.title === NA_TITLE) b.title = "";
    }
  };
  flagNotApplied("fnum", "apertureNorm", ["aperture-dec", "aperture-inc"]);
  flagNotApplied("iso-val", "iso", ["iso-dec", "iso-inc"]);
  flagNotApplied("kelvin-val", "kelvin", ["kelvin-dec", "kelvin-inc"]);
  flagNotApplied("tint-val", "tint", ["tint-dec", "tint-inc"]);
  flagNotApplied("shutter-val", "shutter", ["shutter-dec", "shutter-inc"]);
  flagNotApplied("fps-val", "fps", ["fps-set-grab"]);

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
  // issue 1337: DO NOT drop the whole push while the operator interacts — that made every
  // confirmation arriving during a click sequence invisible (the owner's 2-3 s number lag).
  // updateBlock protects only the control being actively dragged (the activeElement slider check);
  // every value LABEL always reconciles live. (Addendum #1337 replaced the ISO/shutter button groups
  // with index-slider steppers, so there is no button REBUILD left to eat a mid-tap.)
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

// issue 1343: connection model + LOUD offline banner. The panel is "connected" iff the WS is open
// OR the last successful contact (a poll OR a WS push) is younger than CONN_STALE_MS. Otherwise a
// full-width red banner with a live "posledný kontakt pred N s" counter is shown at the top of the
// page — so a frozen last-DOM (a device off the venue LAN whose every fetch fails) is never mistaken
// for a stuck relay. Cleared the instant a push/poll succeeds; the steppers stay enabled (a tap
// still fires a PUT that may land on reconnect — the banner is the truth signal, not a lockout).
const CONN_STALE_MS = 5000;
let lastContactMs = Date.now(); // a fresh page gets a brief grace before the banner can show

function isConnected() {
  // An OPEN WS counts as connected even if it is silent; a half-open (partitioned) socket is
  // resolved by TCP keepalive firing `close` -> scheduleWsReconnect -> updateConnBanner, which is
  // the design's assumption (a genuinely dead socket becomes not-open, then the banner shows).
  return wsConnected || Date.now() - lastContactMs < CONN_STALE_MS;
}

function updateConnBanner() {
  if (!connBanner) return;
  if (isConnected()) {
    connBanner.hidden = true;
  } else {
    const ageS = Math.max(0, Math.floor((Date.now() - lastContactMs) / 1000));
    connBanner.textContent = `Bez spojenia so službou (posledný kontakt pred ${ageS} s)`;
    connBanner.hidden = false;
  }
}

// A successful poll or WS push is "contact" — refresh the age and re-evaluate the banner now.
function markContact() {
  lastContactMs = Date.now();
  updateConnBanner();
}

async function poll() {
  try {
    const r = await fetch("/api/cameras", { cache: "no-store" });
    if (!r.ok) throw new Error("HTTP " + r.status);
    connEl.textContent = "online";
    connEl.classList.remove("bad");
    render(await r.json());
    markContact(); // issue 1343: a successful poll clears the offline banner
  } catch (e) {
    connEl.textContent = "offline";
    connEl.classList.add("bad");
    updateConnBanner(); // issue 1343: a failed poll may reveal the banner (once stale)
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
    markContact(); // issue 1343: an open WS is live contact — clear the offline banner
  });
  sock.addEventListener("message", (ev) => {
    try {
      const msg = JSON.parse(ev.data);
      // Flattened envelope: {type:"state", version, cameras} — render() reads version/cameras.
      if (msg && msg.type === "state") {
        render(msg);
        markContact(); // issue 1343: a live push is contact
      }
    } catch (e) {
      console.warn("bad ws message", e);
    }
  });
  sock.addEventListener("close", () => {
    wsConnected = false;
    scheduleWsReconnect();
    updateConnBanner(); // issue 1343: re-evaluate the banner now the WS is down
  });
  // An error is always followed by close; close drives the reconnect, so nothing to do here.
  // issue 1343: NO console.warn — a WS error while the service is unreachable is the EXPECTED
  // offline path (the loud banner is the signal), and a warning per reconnect would violate
  // browser-console-zero-errors on a legitimately-offline panel.
  sock.addEventListener("error", () => {});
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
// issue 1343: tick the offline banner's live "posledný kontakt pred N s" age counter (and reveal
// it once contact goes stale) independently of the poll cadence.
updateConnBanner();
setInterval(updateConnBanner, 500);

// issue 1305: register the service worker so the panel is installable as a PWA (own icon,
// standalone window in the Windows dock). The SW is a pure network passthrough (no cache —
// server-truth). Guarded on the API being present: on an insecure-origin LAN page (plain http
// on a bare hostname) `navigator.serviceWorker` is undefined, so this is a no-op and the console
// stays clean; the `.catch` swallows any registration error so nothing is logged either way.
if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("/sw.js").catch(() => {});
}
