"use strict";
// bkshading service worker (issue 1305). It exists ONLY to satisfy the browser's PWA-install
// requirement (a registered service worker + a manifest), so the operator can install the panel
// as a windowed app in the Windows dock. It deliberately does NO caching: the panel is
// server-truth (it must never serve a stale UI or stale shading state), so `fetch` is a pure
// passthrough to the network. No Cache Storage API is used anywhere.
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));
self.addEventListener("fetch", (event) => {
  event.respondWith(fetch(event.request));
});
