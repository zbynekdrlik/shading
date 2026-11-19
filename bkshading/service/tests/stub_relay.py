#!/usr/bin/env python3
"""A tiny stdlib stub of a bkshading-relay, for the issue-1304 panel Playwright E2E.

The `bkshading` service polls each camera's relay at `GET /api/state` (-> RelayState JSON) and
forwards operator writes to `PUT /api/params` (a SetRequest JSON body). This stub answers both
WITHOUT a camera or the real relay binary, so the E2E can drive the panel end-to-end on CI:

  * GET  /api/state   -> a fixed RelayState fixture INCLUDING `caps.fNumberChoices` (issue 1304),
                         so the panel can compute the aperture +/- step.
  * PUT  /api/params   -> records the exact JSON body (what the service forwarded) and returns 200.
  * GET  /__recorded   -> the recorded PUT bodies as a JSON array (the test asserts against these).
  * POST /__state      -> merges the JSON body into the served state (shading#14: a test turns the
                         camera offline with an `offlineReason`, or reports an old relay version).
  * POST /__reset      -> serves the original fixture again (and drops a /__lag delay).
  * POST /__lag        -> `{"stateDelayMs": N}`: a `GET /api/state` copies the state when it
                         ARRIVES and answers N ms later (shading#27: a pump poll still in flight
                         while a write lands).
  * GET  /__gets       -> how many `GET /api/state` arrived so far (a test waits for a poll to
                         be in flight).
  * POST /__write      -> `{"delayMs": N, "refuse": bool, "stepAv": x}`: a `PUT /api/params`
                         answers N ms after it arrives (shading#29: the panel's lock while a write
                         is unanswered), and with `refuse` it applies nothing and answers the state
                         with every written key in `notApplied` (the camera refused it); `stepAv`
                         sets the AV one `apertureStep` moves (default APERTURE_STEP_AV). Dropped
                         by /__reset.
  * `--apply-writes`   -> a PUT changes the served params (iso, kelvin, tint, shutter) and answers
                         `{"applied": n, "state": {...}}` like the real relay, so the service pushes
                         its immediate confirmation (issue 1337). An `apertureStep` of +/-1 moves
                         `apertureAv` by one fine lens step (APERTURE_STEP_AV = 0.1 AV, the
                         relay's live behaviour, shading#29; `/__write` `stepAv` changes it). Off
                         by default: the shared stub keeps its fixed fixture.

Stdlib only (http.server/json/argparse), threaded so the service's concurrent poll + the browser
never block each other. Fails loudly on error (never a silent swallow, per script-failure-policy).
"""
import argparse
import copy
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The RelayState fixture the service deserializes (camelCase wire, matching bkshading-proto).
# apertureNorm = 2/3 -> f-number choice index 2 (f/5.2) of 4; a "+" tap sends apertureStep 1
# (shading#6: the relay walks the lens's own steps; this stub only records the body). kelvin 5600 (+100 -> 5700), tint 0 (+1 -> 1).
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
        "irisRangePos": 0.5,
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
_state = copy.deepcopy(STATE)
_state_delay_ms = 0
_write_delay_ms = 0
_write_refuse = False
_step_av = None
_gets = 0
_apply_writes = False
# The params a PUT may change when --apply-writes is on.
_WRITABLE = ("iso", "kelvin", "tint", "shutter")
# The AV one `apertureStep` moves under --apply-writes: the lens's fine step (shading#29).
APERTURE_STEP_AV = 0.1


class Handler(BaseHTTPRequestHandler):
    def _json(self, code, obj):
        body = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        global _gets
        if self.path == "/api/state":
            with _lock:
                _gets += 1
                snap = copy.deepcopy(_state)
                delay_ms = _state_delay_ms
            if delay_ms:
                time.sleep(delay_ms / 1000.0)
            self._json(200, snap)
        elif self.path == "/__gets":
            with _lock:
                self._json(200, {"gets": _gets})
        elif self.path == "/__recorded":
            with _lock:
                self._json(200, list(_recorded))
        else:
            self._json(404, {"error": "not found", "path": self.path})

    def do_POST(self):
        global _state, _state_delay_ms, _write_delay_ms, _write_refuse, _step_av
        if self.path == "/__state":
            length = int(self.headers.get("Content-Length", "0"))
            patch = json.loads(self.rfile.read(length).decode("utf-8"))  # loud on malformed JSON
            with _lock:
                _state.update(patch)
                self._json(200, _state)
        elif self.path == "/__reset":
            with _lock:
                _state = copy.deepcopy(STATE)
                _state_delay_ms = 0
                _write_delay_ms = 0
                _write_refuse = False
                _step_av = None
                self._json(200, _state)
        elif self.path == "/__lag":
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(length).decode("utf-8"))  # loud on malformed JSON
            with _lock:
                _state_delay_ms = int(body["stateDelayMs"])
                self._json(200, {"stateDelayMs": _state_delay_ms})
        elif self.path == "/__write":
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(length).decode("utf-8"))  # loud on malformed JSON
            with _lock:
                _write_delay_ms = int(body.get("delayMs", 0))
                _write_refuse = bool(body.get("refuse", False))
                _step_av = float(body["stepAv"]) if body.get("stepAv") is not None else None
                self._json(
                    200,
                    {"delayMs": _write_delay_ms, "refuse": _write_refuse, "stepAv": _step_av},
                )
        else:
            self._json(404, {"error": "not found", "path": self.path})

    def do_PUT(self):
        if self.path == "/api/params":
            length = int(self.headers.get("Content-Length", "0"))
            raw = self.rfile.read(length) if length else b"{}"
            body = json.loads(raw.decode("utf-8"))  # loud on malformed JSON
            with _lock:
                _recorded.append(body)
                delay_ms = _write_delay_ms
                refuse = _write_refuse
            # Outside the lock: the service's state polls keep answering meanwhile.
            if delay_ms:
                time.sleep(delay_ms / 1000.0)
            with _lock:
                if refuse:
                    answer = copy.deepcopy(_state)
                    answer["notApplied"] = [k for k, v in body.items() if v is not None]
                    self._json(200, {"applied": 0, "state": answer})
                    return
                if not _apply_writes:
                    self._json(200, {"ok": True})
                    return
                applied = 0
                for key in _WRITABLE:
                    if body.get(key) is not None:
                        _state["params"][key] = body[key]
                        applied += 1
                step = body.get("apertureStep")
                if step is not None:
                    sign = 1 if step > 0 else -1
                    step_av = APERTURE_STEP_AV if _step_av is None else _step_av
                    av = _state["params"]["apertureAv"] + sign * step_av
                    _state["params"]["apertureAv"] = round(av, 6)
                    applied += 1
                self._json(200, {"applied": applied, "state": _state})
        else:
            self._json(404, {"error": "not found", "path": self.path})

    def log_message(self, *_args):
        pass  # quiet: the E2E orchestrator owns the logs


def main():
    ap = argparse.ArgumentParser(description="bkshading stub relay for the issue-1304 panel E2E")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--apply-writes", action="store_true", help="a PUT changes the served params")
    args = ap.parse_args()
    global _apply_writes
    _apply_writes = args.apply_writes
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"stub relay listening on http://{args.host}:{args.port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
