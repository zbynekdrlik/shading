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


def _run():
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for fn in fns:
        fn()
        print(f"ok  {fn.__name__}")
    print(f"\n{len(fns)} passed")


if __name__ == "__main__":
    _run()
