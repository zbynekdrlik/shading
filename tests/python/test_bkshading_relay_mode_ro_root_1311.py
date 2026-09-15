"""Tier-0: the EVENT-only relay lifecycle must PERSIST on the read-only cambox root (issue 1311).

Live 14.9.2026 on cam1 (ro root): `rig-mode.sh test` printed "stopped+disabled" while
`systemctl disable` had silently failed with `Read-only file system` (2>/dev/null || true) -- the
unit stayed `enabled`, so the next boot would re-arm the relay (the USB-event source Finding 2
removes). The stop/start command builders must wrap the enable-state change in a remount-rw
window, read the state back, and the emitted script must FAIL LOUD when the state did not land.
"""

import os
import stat
import subprocess
import tempfile
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LIB = os.path.join(REPO, "scripts", "lib", "bkshading-relay-mode.sh")


def _cmds(fn: str) -> str:
    r = subprocess.run(["bash", "-c", f'set -u; . "{LIB}"; {fn}'], capture_output=True, text=True, timeout=20)
    assert r.returncode == 0, r.stderr
    return r.stdout


def _fake_bin(d: str, name: str, body: str) -> None:
    p = os.path.join(d, name)
    with open(p, "w") as fh:
        fh.write("#!/usr/bin/env bash\n" + body)
    os.chmod(p, os.stat(p).st_mode | stat.S_IEXEC)


class StopCmdsShape(unittest.TestCase):
    def test_disable_and_enable_sit_inside_a_remount_rw_window_and_read_back(self):
        for fn, verb in (("bkshading_relay_mode_stop_cmds", "systemctl disable bkshading-relay.service"),
                         ("bkshading_relay_mode_start_cmds", "systemctl enable bkshading-relay.service")):
            s = _cmds(fn)
            rw, ch, ro = s.find("remount,rw"), s.find(verb), s.find("remount,ro")
            self.assertTrue(0 <= rw < ch < ro, f"{fn}: want remount,rw < {verb!r} < remount,ro; got {rw},{ch},{ro}\n{s}")
            self.assertIn("is-enabled", s, f"{fn} must read the persistent state back")
            self.assertIn("RELAY_ENABLED=", s, f"{fn} must emit the read-back for the caller")


class StopCmdsBehaviour(unittest.TestCase):
    def _run(self, fn: str, disable_fails: bool):
        with tempfile.TemporaryDirectory() as d:
            log = os.path.join(d, "calls.log")
            _fake_bin(d, "mount", f'echo "mount $*" >> "{log}"\n')
            state = os.path.join(d, "state")
            with open(state, "w") as fh:
                fh.write("enabled\n")
            fail = "exit 1" if disable_fails else f'echo disabled > "{state}"'
            _fake_bin(d, "systemctl", f'''echo "systemctl $*" >> "{log}"
case "$1" in
  is-enabled) cat "{state}"; exit 0 ;;
  disable) {fail} ;;
  enable) echo enabled > "{state}" ;;
  stop|start) : ;;
esac
exit 0
''')
            env = {**os.environ, "PATH": d + ":" + os.environ["PATH"]}
            r = subprocess.run(["bash", "-c", _cmds(fn)], capture_output=True, text=True, timeout=20, env=env)
            calls = open(log).read().splitlines()
            return r, calls

    def test_stop_disables_inside_the_window_and_reports_disabled(self):
        r, calls = self._run("bkshading_relay_mode_stop_cmds", disable_fails=False)
        self.assertEqual(r.returncode, 0, r.stderr)
        mounts = [c for c in calls if c.startswith("mount")]
        self.assertEqual(mounts[0], "mount -o remount,rw /", calls)
        self.assertEqual(mounts[-1], "mount -o remount,ro /", calls)
        self.assertLess(calls.index("systemctl stop bkshading-relay.service"), calls.index("systemctl disable bkshading-relay.service"), calls)
        self.assertIn("RELAY_ENABLED=disabled", r.stdout)

    def test_stop_fails_loud_when_the_disable_did_not_land(self):
        r, calls = self._run("bkshading_relay_mode_stop_cmds", disable_fails=True)
        self.assertNotEqual(r.returncode, 0, "a persist that did not land must not exit 0")
        self.assertIn("RELAY_ENABLED=enabled", r.stdout + r.stderr)
        self.assertIn("mount -o remount,ro /", calls, "the ro restore must run even on failure")


if __name__ == "__main__":
    unittest.main()
