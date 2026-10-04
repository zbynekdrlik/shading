#!/usr/bin/env python3
"""bkshading-panel-app (issue 1399): the shading panel as an app window on strih-lx.

WHY (owner, 4.10.2026: "na strih nb ma po starte bezat shading appka" and "nevie to byt pekna pwa
appka lebo tam v prehliadacoch je ta hnusna horna lista, a stale tak vyskakuju nejake blbosti"): the
bkshading service (:8770) ran, but nothing showed its panel, and a browser window brings an address
bar, translation popups and privacy tabs.

WHAT (design comment 5977447339): ONE Gtk 3 window titled "Shading" holding a WebKit2 4.1 WebView
on http://127.0.0.1:8770/ -- no browser chrome, no context menu, no developer extras, no new windows
(the `create` signal returns None, scripts cannot open windows). A failed load or a crashed web
process reloads the panel after RETRY_DELAY_S (a failed load shows no WebKit error page).
systemd/bkshading-panel-app.service supervises it (Restart=always, DISPLAY=:0); the kiosk openbox
autostart starts it at every login.

It is a NORMAL window -- never keep-above / keep-below and no special type hint -- so openbox's
Alt+Tab reaches it (the live 4.10. mistake: a `below` window Alt+Tab could not bring up). The
WM_CLASS comes from GLib.set_prgname (Gtk.Window.set_wmclass is deprecated).

The toolkit modules are passed into PanelWindow, so tests/python/test_strih_session_apps_1399.py
drives the real wiring with stand-in Gtk / WebKit2 / GLib objects (no display in CI).
"""
import argparse
import os
import sys

PANEL_URL = "http://127.0.0.1:8770/"
WINDOW_TITLE = "Shading"
DEFAULT_SIZE = (1280, 980)
RETRY_DELAY_S = 3
PRGNAME = "bkshading-panel-app"


def _log(msg):
    print(msg, flush=True)


def is_cancelled_load(error, WebKit2):
    """True for a load that was cancelled because another load started (the retry itself, or the
    page navigating): no error page is shown for it and retrying it would only loop."""
    try:
        return bool(error.matches(WebKit2.NetworkError.quark(), WebKit2.NetworkError.CANCELLED))
    except AttributeError:
        return False


class PanelWindow:
    """The panel window + its reload policy. Gtk / WebKit2 / GLib are the gi.repository modules in
    production; tests pass stand-ins with the same calls."""

    def __init__(self, Gtk, WebKit2, GLib, url=PANEL_URL, log=_log):
        self._gtk = Gtk
        self._webkit = WebKit2
        self._glib = GLib
        self.url = url
        self._log = log
        self.retry_pending = False

        win = Gtk.Window(title=WINDOW_TITLE)
        win.set_default_size(*DEFAULT_SIZE)
        win.connect("destroy", self._on_destroy)

        view = WebKit2.WebView()
        settings = view.get_settings()
        settings.set_javascript_can_open_windows_automatically(False)
        settings.set_enable_developer_extras(False)
        view.connect("context-menu", self._on_context_menu)
        view.connect("create", self._on_create)
        view.connect("load-failed", self._on_load_failed)
        view.connect("web-process-terminated", self._on_web_process_terminated)
        win.add(view)
        self.window = win
        self.view = view

    def start(self):
        self.window.show_all()
        self._log("bkshading-panel-app: loading %s" % self.url)
        self.view.load_uri(self.url)

    def _schedule_retry(self, why):
        """One pending reload at a time: a burst of failures never stacks reloads."""
        if self.retry_pending:
            return
        self.retry_pending = True
        self._log("bkshading-panel-app: %s -- reloading %s in %d s" % (why, self.url, RETRY_DELAY_S))
        self._glib.timeout_add_seconds(RETRY_DELAY_S, self._retry)

    def _retry(self):
        self.retry_pending = False
        self.view.load_uri(self.url)
        return False  # a one-shot GLib timeout

    def _on_destroy(self, *_args):
        # The operator closed the window: exit; the unit's Restart=always brings it back.
        self._log("bkshading-panel-app: window closed -- exiting (the unit restarts it)")
        self._gtk.main_quit()

    def _on_context_menu(self, *_args):
        return True  # handled: no context menu

    def _on_create(self, *_args):
        return None  # no new web view, so no new window

    def _on_load_failed(self, _view, _event, uri, error):
        if is_cancelled_load(error, self._webkit):
            return True
        message = getattr(error, "message", None) or str(error)
        self._schedule_retry("load of %s failed (%s)" % (uri, message))
        return True  # handled: no WebKit error page

    def _on_web_process_terminated(self, _view, reason):
        self._schedule_retry("web process terminated (%s)" % (reason,))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--url", default=PANEL_URL)
    a = ap.parse_args(argv)
    import gi
    gi.require_version("Gtk", "3.0")
    gi.require_version("WebKit2", "4.1")
    from gi.repository import GLib

    # Before Gtk is imported: PyGObject initialises GDK on that import, and GDK takes the WM_CLASS
    # class from the program name it sees then.
    GLib.set_prgname(PRGNAME)
    GLib.set_application_name(WINDOW_TITLE)
    from gi.repository import Gtk, WebKit2

    ok, _rest = Gtk.init_check(sys.argv)
    if not ok:
        _log("bkshading-panel-app: no X display (DISPLAY=%r) -- exiting, the unit retries" % (
            os.environ.get("DISPLAY"),))
        return 1
    PanelWindow(Gtk, WebKit2, GLib, url=a.url).start()
    Gtk.main()
    return 0


if __name__ == "__main__":
    sys.exit(main())
