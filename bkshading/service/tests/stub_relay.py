#!/usr/bin/env python3
"""A tiny stdlib stub of a bkshading-relay, for the issue-1304 panel Playwright E2E.

The `bkshading` service polls each camera's relay at `GET /api/state` (-> RelayState JSON) and
forwards operator writes to `PUT /api/params` (a SetRequest JSON body). This stub answers both
WITHOUT a camera or the real relay binary, so the E2E can drive the panel end-to-end on CI:

  * GET  /api/state   -> a fixed RelayState fixture INCLUDING `caps.fNumberChoices` (issue 1304),
                         so the panel can compute the aperture +/- step.
  * PUT  /api/params   -> records the exact JSON body (what the service forwarded) and returns 200.
  * GET  /__recorded   -> the recorded PUT bodies as a JSON array (the test asserts against these).

Stdlib only (http.server/json/argparse), threaded so the service's concurrent poll + the browser
never block each other. Fails loudly on error (never a silent swallow, per script-failure-policy).
"""
import argparse
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The RelayState fixture the service deserializes (camelCase wire, matching bkshading-proto).
# apertureNorm = 2/3 -> f-number choice index 2 (f/5.2) of 4; a "+" tap steps to index 3 (f/8.0),
# i.e. an absolute apertureNorm of 1.0. kelvin 5600 (+100 -> 5700), tint 0 (+1 -> 1).
STATE = {
    "online": True,
    "camera": "Blackmagic Design Pocket Cinema Camera 4K",
    "params": {
        "apertureAv": 4.78,
        "apertureNorm": 2.0 / 3.0,
        "iso": 400,
        "kelvin": 5600,
        "tint": 0,
        "shutter": 50,
        "fps100": 6000,
        "sensorFps100": 6000,
        "focusDistance": None,
    },
    "caps": {
        "isoChoices": [100, 200, 400, 800],
        "fNumberChoices": [2.8, 4.0, 5.2, 8.0],
        "shutterChoices": [60, 100, 125],
        "fpsMin": 5,
        "fpsMax": 60,
        "kelvinMin": 2500,
        "kelvinMax": 10000,
    },
    "fpsSupported": True,
    "captureFps": None,
    "version": "1.7.0-dev.e2e",
}

_recorded = []
_lock = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    def _json(self, code, obj):
        body = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/api/state":
            self._json(200, STATE)
        elif self.path == "/__recorded":
            with _lock:
                self._json(200, list(_recorded))
        else:
            self._json(404, {"error": "not found", "path": self.path})

    def do_PUT(self):
        if self.path == "/api/params":
            length = int(self.headers.get("Content-Length", "0"))
            raw = self.rfile.read(length) if length else b"{}"
            body = json.loads(raw.decode("utf-8"))  # loud on malformed JSON
            with _lock:
                _recorded.append(body)
            self._json(200, {"ok": True})
        else:
            self._json(404, {"error": "not found", "path": self.path})

    def log_message(self, *_args):
        pass  # quiet: the E2E orchestrator owns the logs


def main():
    ap = argparse.ArgumentParser(description="bkshading stub relay for the issue-1304 panel E2E")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    args = ap.parse_args()
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"stub relay listening on http://{args.host}:{args.port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
