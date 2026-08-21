"use strict";
// bkshading web panel (issue 808). Server-truth: the panel renders whatever /api/cameras
// reports and never keeps optimistic local state. Controls PUT a shading change to
// /api/cameras/<id>/params (forwarded to the camera's relay). M2: a camera with an NDI
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
    // A failed write surfaces on the next poll (server-truth); no optimistic UI.
    console.warn("set failed", id, e);
  }
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
  q("aperture").addEventListener("change", guard((e) => setParam(id, { apertureNorm: Number(e.target.value) })));
  q("kelvin").addEventListener("change", guard((e) => setParam(id, { kelvin: Math.round(Number(e.target.value)) })));
  q("tint").addEventListener("change", guard((e) => setParam(id, { tint: Math.round(Number(e.target.value)) })));
  q("auto-wb").addEventListener("click", () => setParam(id, { autoWb: true }));

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

  // Aperture.
  const fn = fNumberFromAv(p.apertureAv);
  q("fnum").textContent = fn == null ? "f/—" : "f/" + fn.toFixed(1);
  const apEl = q("aperture");
  if (document.activeElement !== apEl && p.apertureNorm != null) apEl.value = p.apertureNorm;

  // ISO.
  q("iso-val").textContent = p.iso == null ? "—" : String(p.iso);
  renderButtonGroup(q("iso"), caps ? caps.isoChoices : [], p.iso, (v) => setParam(cam.id, { iso: v }));

  // White balance.
  q("kelvin-val").textContent = p.kelvin == null ? "—" : p.kelvin + "K";
  const kEl = q("kelvin");
  if (document.activeElement !== kEl && p.kelvin != null) kEl.value = p.kelvin;
  q("tint-val").textContent = p.tint == null ? "—" : String(p.tint);
  const tEl = q("tint");
  if (document.activeElement !== tEl && p.tint != null) tEl.value = p.tint;

  // Shutter.
  q("shutter-val").textContent = p.shutter == null ? "—" : "1/" + p.shutter;
  renderButtonGroup(q("shutter"), caps ? caps.shutterChoices : [], p.shutter, (v) => setParam(cam.id, { shutter: v }));

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
  if (interacting) return;
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
