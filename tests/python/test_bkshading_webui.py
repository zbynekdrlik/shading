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


def _run():
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
    print(f"\n{len(fns)} passed")


if __name__ == "__main__":
    _run()
