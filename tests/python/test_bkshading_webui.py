#!/usr/bin/env python3
"""Static verification of the bkshading web panel (issue 808, M1 skeleton + M2 live preview).

The service can't be built locally (camera-box Tier-0 bans cargo build; CI is the first
compile), so this checks the SHIPPED web assets structurally: the 4+4 block skeleton
(preview on top, params below), the M2 live preview <img> + its /api/cameras/<id>/preview.jpg
source, the version-on-dashboard label, the shading controls, and that the JS talks to the
real service API and carries no localhost URLs. Runnable directly
(`python3 test_bkshading_webui.py`) or under pytest.
"""
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
WEB = os.path.join(HERE, "..", "..", "bkshading", "service", "web")


def _read(name):
    with open(os.path.join(WEB, name), encoding="utf-8") as fh:
        return fh.read()


def test_index_has_versioned_block_skeleton():
    html = _read("index.html")
    # version-on-dashboard: build-time placeholder + testable label.
    assert "{{VERSION}}" in html, "version placeholder must be present for build-time injection"
    assert 'data-testid="version"' in html
    assert "v{{VERSION}}" in html, "displayed version must be v-prefixed (v<semver>)"
    # The 4+4 block skeleton: a per-camera block template with a preview area ON TOP and the
    # shading parameters BELOW it.
    assert '<template id="camera-block">' in html
    assert 'data-role="preview"' in html, "each block has a preview area on top"
    # M2: the preview area carries a live <img> plus a placeholder shown until the first frame.
    assert 'data-role="preview-img"' in html, "M2 live preview image element"
    assert 'data-role="preview-placeholder"' in html, "placeholder until a frame arrives"
    assert 'data-role="params"' in html
    # The block's preview appears before its params (top vs bottom).
    assert html.index('data-role="preview"') < html.index('data-role="params"')
    # The grid the blocks render into.
    assert 'id="camera-grid"' in html


def test_app_js_wires_the_live_preview_endpoint():
    js = _read("app.js")
    # M2: the preview <img> is reloaded from the per-camera JPEG endpoint.
    assert re.search(r"/api/cameras/\$\{[^}]+\}/preview\.jpg", js), "preview <img> hits preview.jpg"
    assert 'data-role="preview-img"' in js, "the JS reloads the preview image element"


def test_app_js_uses_websocket_live_push_with_http_fallback():
    # issue 808 WS milestone: the panel's primary live channel is a WebSocket to /ws; it renders
    # the flattened {"type":"state",...} envelope, and HTTP /api/cameras polling stays as a
    # fallback that runs only while the WS is down.
    js = _read("app.js")
    assert "new WebSocket(" in js, "panel opens a WebSocket for live push"
    assert "/ws" in js, "the live channel is the /ws endpoint"
    assert 'msg.type === "state"' in js, "renders the flattened state envelope"
    # wss/ws chosen from the page protocol (works behind the cloudflare https remote).
    assert '"wss:"' in js and '"ws:"' in js, "ws/wss selected from location.protocol"
    # The HTTP poll must remain, gated on the WS being down (fallback, not the primary path).
    assert "if (!wsConnected) poll()" in js, "HTTP poll runs only as a WS fallback"
    # A dropped WS reconnects with backoff (never a dead panel).
    assert "scheduleWsReconnect" in js, "WS reconnects on drop"


def test_index_has_all_shading_controls():
    html = _read("index.html")
    for role in ("aperture", "iso", "kelvin", "tint", "shutter", "fps-val", "auto-wb"):
        assert f'data-role="{role}"' in html, f"missing shading control: {role}"


def test_app_js_uses_real_service_api_only():
    js = _read("app.js")
    assert "/api/cameras" in js, "panel polls the aggregate endpoint"
    assert re.search(r"/api/cameras/\$\{[^}]+\}/params", js), "controls PUT to the per-camera endpoint"
    assert '"PUT"' in js
    # server-truth model, no optimistic local state.
    assert "server-truth" in js.lower() or "server truth" in js.lower()


def test_no_localhost_urls_anywhere():
    for name in ("index.html", "app.js", "style.css"):
        text = _read(name)
        for bad in ("localhost", "127.0.0.1", "0.0.0.0"):
            assert bad not in text, f"{name} must not hardcode {bad}"


def test_index_has_fps_grab_sync_ui():
    # issue 809: each block shows the box grab fps, a mismatch warning, and an explicit
    # "align to grab" button (never an auto-write).
    html = _read("index.html")
    for role in ("fps-grab", "fps-warn", "fps-set-grab"):
        assert f'data-role="{role}"' in html, f"missing fps-sync element: {role}"
    # the warning + align button are hidden by default (shown only on a mismatch).
    assert 'data-role="fps-warn" hidden' in html, "fps-warn hidden by default"
    assert 'data-role="fps-set-grab" hidden' in html, "align button hidden by default"


def test_app_js_align_button_sends_grab_fps_without_autowrite():
    # issue 809: the align button issues an explicit fps write of the configured grab value,
    # and there is NO automatic fps write anywhere (operator action only). The negative claim
    # is verified, not just asserted-in-prose: the sole fps write must sit inside a click
    # handler, and updateBlock() (which runs on every 2s poll) must never write fps.
    js = _read("app.js")
    assert "fps-set-grab" in js, "JS wires the align button (q('fps-set-grab'))"
    assert "cam.fpsSync" in js, "JS renders the fps sync verdict"
    assert "cam.grabFps" in js, "JS renders the configured grab fps"
    # exactly ONE fps write in the whole panel — the explicit align button.
    assert js.count("{ fps:") == 1, "exactly one fps write (the explicit align button)"
    idx = js.index("{ fps:")
    before = js[:idx]
    # that single write's nearest enclosing listener is the CLICK handler (not a change/poll).
    last_listener = before.rfind("addEventListener(")
    assert last_listener != -1 and before[last_listener:].startswith(
        'addEventListener("click"'
    ), "the fps write must live inside a click handler, not an auto path"
    # updateBlock() runs every poll; it must never write fps (that would be an auto-write).
    ub = js.index("function updateBlock(")
    assert "{ fps:" not in js[ub:], "updateBlock must never write fps (no auto-write)"


def test_index_and_js_surface_grab_config_desync():
    # issue 809 remainder: the panel surfaces when the static grab_fps config disagrees with the
    # box's live capture rate (a silent desync after a capture-mode change).
    html = _read("index.html")
    assert 'data-role="fps-desync"' in html, "desync element present in the block template"
    assert 'data-role="fps-desync" hidden' in html, "desync hidden by default (shown on desync)"
    js = _read("app.js")
    assert "cam.grabFpsDesync" in js, "the JS renders the grab config desync flag"
    assert 'q("fps-desync")' in js, "the JS drives the desync element"


def _js_fn_body(js, sig):
    """The brace-balanced body of the JS function whose declaration starts with `sig`.
    Naive brace counting is fine here: every `{`/`}` in these functions is balanced
    (object literals, `${...}` template holes), so depth never goes wrong."""
    start = js.index(sig)
    open_brace = js.index("{", start)
    depth = 0
    for i in range(open_brace, len(js)):
        c = js[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return js[open_brace : i + 1]
    raise AssertionError("unbalanced braces for " + sig)


def test_index_has_aperture_kelvin_tint_step_buttons_1304():
    # issue 1304: each of clona / biely bod / tint gets a - and a + step button next to its slider.
    html = _read("index.html")
    for role in (
        "aperture-dec",
        "aperture-inc",
        "kelvin-dec",
        "kelvin-inc",
        "tint-dec",
        "tint-inc",
    ):
        assert f'data-role="{role}"' in html, f"missing step button: {role}"
    # The layout is - [slider] + : a .slider-row wraps each slider + its two step buttons.
    assert "slider-row" in html


def test_app_js_step_handlers_send_absolute_values_1304():
    # issue 1304: the aperture step sends apertureNorm; kelvin/tint steps send kelvin/tint.
    js = _read("app.js")
    ap = _js_fn_body(js, "function stepAperture(")
    assert "{ apertureNorm: norm }" in ap, "aperture step sends an absolute apertureNorm"
    lin = _js_fn_body(js, "function stepLinear(")
    assert "{ [key]: next }" in lin, "linear step sends an absolute value for its key"
    # the step buttons are wired to kelvin/tint via stepLinear.
    assert '"kelvin", "kelvin"' in js and '"tint", "tint"' in js


def test_app_js_step_handlers_have_no_repeat_timer_1304():
    # pin bodu 3: one tap = one PUT, NO auto-repeat on hold. The step handlers must contain no
    # setInterval/setTimeout, and the buttons must be wired as CLICK handlers (not a held repeat).
    js = _read("app.js")
    for sig in ("function stepAperture(", "function stepLinear("):
        body = _js_fn_body(js, sig)
        assert "setInterval" not in body, f"{sig} must not auto-repeat (no setInterval)"
        assert "setTimeout" not in body, f"{sig} must not auto-repeat (no setTimeout)"
    for role in ("aperture-dec", "aperture-inc", "kelvin-dec", "kelvin-inc", "tint-dec", "tint-inc"):
        assert re.search(
            r'q\("' + re.escape(role) + r'"\)\.addEventListener\("click"', js
        ), f"{role} must be a click handler (no pointerdown-hold repeat)"


def test_app_js_aperture_step_disabled_without_choices_1304():
    # issue 1304 pin: without f-number choices the aperture +/- is DISABLED (never a fabricated
    # step), and the choices are read from caps.fNumberChoices exposed on the block dataset.
    js = _read("app.js")
    assert "caps.fNumberChoices" in js, "panel reads the f-number choices from caps"
    assert "refreshStepDisabled" in js, "panel enables/disables the step buttons"
    rd = _js_fn_body(js, "function refreshStepDisabled(")
    assert "disabled = true" in rd, "step buttons disabled when no choices / at a bound"


def test_index_links_pwa_manifest_and_icons_1305():
    html = _read("index.html")
    assert '<link rel="manifest"' in html, "index links a web app manifest"
    assert "manifest.webmanifest" in html
    assert '<meta name="theme-color"' in html
    assert '<link rel="icon"' in html
    assert "apple-touch-icon" in html


def test_manifest_is_valid_standalone_pwa_1305():
    import json

    with open(os.path.join(WEB, "manifest.webmanifest"), encoding="utf-8") as fh:
        m = json.load(fh)
    assert m["display"] == "standalone"
    assert m["start_url"] == "/"
    assert m["scope"] == "/"
    srcs = [i.get("src") for i in m["icons"]]
    assert "/icon-192.png" in srcs
    assert "/icon-512.png" in srcs
    assert any("maskable" in (i.get("purpose") or "") for i in m["icons"]), "a maskable icon entry"


def test_service_worker_is_passthrough_no_cache_1305():
    with open(os.path.join(WEB, "sw.js"), encoding="utf-8") as fh:
        sw = fh.read()
    # server-truth: no cache anywhere (no stale UI/state).
    assert "caches" not in sw, "sw.js must not use the Cache Storage API"
    assert "fetch(event.request)" in sw, "sw.js is a pure network passthrough"


def test_app_js_registers_service_worker_guarded_1305():
    js = _read("app.js")
    assert '"serviceWorker" in navigator' in js, "SW registration is guarded"
    assert 'navigator.serviceWorker.register("/sw.js")' in js
    assert ".catch(" in js, "registration errors are swallowed (clean console on insecure origin)"


def test_pwa_icons_are_png_1305():
    for name in ("icon-192.png", "icon-512.png"):
        with open(os.path.join(WEB, name), "rb") as fh:
            assert fh.read(8) == b"\x89PNG\r\n\x1a\n", f"{name} is not a PNG"
    with open(os.path.join(WEB, "favicon.svg"), encoding="utf-8") as fh:
        assert "<svg" in fh.read(), "favicon.svg is an SVG"


def _run():
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
    print(f"\n{len(fns)} passed")
def test_app_js_aperture_steps_from_real_fnumber_1337():
    # issue 1337: the aperture step must start from the camera's REAL current f-number (an off-grid
    # lens reports slider norm 0, which the old code stepped from -> "clona sa nezdvihne"). The panel
    # carries a JS `stepChoice` mirror of the proto step_choice, steps from `dataset.apertureFnum`,
    # and updateBlock stores that dataset from apertureAv.
    js = _read("app.js")
    assert "function stepChoice(" in js, "JS mirror of proto step_choice present"
    ap = _js_fn_body(js, "function stepAperture(")
    assert "dataset.apertureFnum" in ap, "stepAperture steps from the real f-number, not the norm"
    assert "stepChoice(" in ap, "stepAperture uses the stepChoice mirror"
    # updateBlock stores the real f-number on the dataset for the step handler + disable logic.
    ub = _js_fn_body(js, "function updateBlock(")
    assert "dataset.apertureFnum" in ub, "updateBlock stores the current f-number on the dataset"
    # the current f-number is read through the currentFnum() helper (guards the Number("")===0 trap),
    # which reads dataset.apertureFnum -- not the off-grid slider norm.
    cf = _js_fn_body(js, "function currentFnum(")
    assert "dataset.apertureFnum" in cf, "currentFnum reads the real f-number from the dataset"
    # the disable logic derives the aperture bounds from the real f-number, not the slider norm.
    rd = _js_fn_body(js, "function refreshStepDisabled(")
    assert "currentFnum(" in rd, "aperture bounds come from the real f-number (currentFnum), not the norm"


def test_app_js_optimistic_pending_echo_1337():
    # issue 1337: a click shows its new value immediately as "pending" (reconciled by the next push).
    js = _read("app.js")
    ap = _js_fn_body(js, "function stepAperture(")
    assert 'classList.add("pending")' in ap, "aperture step shows an optimistic pending value"
    lin = _js_fn_body(js, "function stepLinear(")
    assert 'classList.add("pending")' in lin, "kelvin/tint step shows an optimistic pending value"
    # updateBlock RECONCILES: it removes the pending class when it renders the authoritative value.
    ub = _js_fn_body(js, "function updateBlock(")
    assert 'classList.remove("pending")' in ub, "the next push reconciles (clears) the pending value"
    # the pending state is visibly styled.
    css = _read("style.css")
    assert ".pending" in css, "the pending value is visibly styled"


def test_app_js_render_no_longer_drops_whole_push_while_interacting_1337():
    # issue 1337: render must NOT `return` on `interacting` (that dropped every confirmation during a
    # click sequence -> the owner's 2-3 s number lag). Instead updateBlock guards ONLY the button
    # REBUILD (which could eat a mid-tap) with `!interacting`; value labels always reconcile.
    js = _read("app.js")
    rb = _js_fn_body(js, "function render(")
    assert "if (interacting) return" not in rb, "render must not drop the whole push while interacting"
    ub = _js_fn_body(js, "function updateBlock(")
    assert "if (!interacting)" in ub, "only the button rebuild is guarded, not the whole render"
    # one tap is still one PUT with no auto-repeat (the issue-1229 bus doctrine is unchanged).
    for sig in ("function stepAperture(", "function stepLinear("):
        body = _js_fn_body(js, sig)
        assert "setInterval" not in body and "setTimeout" not in body, f"{sig} must not auto-repeat"

if __name__ == "__main__":
    _run()
