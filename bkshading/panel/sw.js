// bkshading panel service worker (shading#16; the doctrine of issue 1305 and fohmixer-ui's sw.js):
// NETWORK-ONLY. It exists only so the panel is an installable PWA (a registered worker + a
// manifest); it never stores or replays an answer, because the panel shows live camera state and
// must never show a stale UI. No Cache Storage anywhere. The /ws socket is not a fetch and never
// passes through here. Only this origin's requests pass through it.
self.addEventListener("install", function () {
    self.skipWaiting();
});
self.addEventListener("activate", function (event) {
    event.waitUntil(self.clients.claim());
});
self.addEventListener("fetch", function (event) {
    if (new URL(event.request.url).origin !== self.location.origin) {
        return;
    }
    event.respondWith(fetch(event.request));
});
