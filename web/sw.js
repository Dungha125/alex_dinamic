// Offline shell for the Alex web app. API calls (other origin), latest.json and
// downloads always go to the network.
const CACHE = "alex-web-v4";
const SHELL = ["./", "index.html", "style.css", "app.js", "qr.js", "qrdecode.js", "config.js", "manifest.webmanifest", "icons/icon-192.png", "icons/icon-512.png"];

self.addEventListener("install", e => {
  e.waitUntil(caches.open(CACHE).then(c => c.addAll(SHELL)).then(() => self.skipWaiting()));
});
self.addEventListener("activate", e => {
  e.waitUntil(caches.keys().then(keys => Promise.all(keys.filter(k => k !== CACHE).map(k => caches.delete(k)))).then(() => self.clients.claim()));
});
self.addEventListener("fetch", e => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET" || url.origin !== location.origin) return;
  if (url.pathname.endsWith("latest.json") || url.pathname.includes("/download/")) return;
  // network first, fall back to cache (so new deployments show up immediately)
  e.respondWith(
    fetch(e.request)
      .then(r => { const copy = r.clone(); if (r.ok) caches.open(CACHE).then(c => c.put(e.request, copy)); return r; })
      .catch(() => caches.match(e.request, { ignoreSearch: true }).then(r => r || caches.match("index.html")))
  );
});
