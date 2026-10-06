// IEM Mixer Service Worker — PWA shell + hashed asset caching.
// Only content-hashed files (WASM/JS from Trunk) are cached.
// index.html and unhashed files are NEVER cached in SW (caused blank pages 2026-03-19).

const CACHE_NAME = 'iem-assets-v1';
// Trunk outputs the loader and the module as iem-ui-c72f48fccb666eb9.js and
// iem-ui-c72f48fccb666eb9_bg.wasm; the hash is an unpadded hex u64, so it can
// have fewer than 16 digits (15 in about one build of 16).
const HASH_RE = /\/iem-ui-[0-9a-f]{1,16}(_bg)?\.(js|wasm)$/;

// Whether this worker replaces an older one (an upgrade, not a first visit).
let upgrading = false;

self.addEventListener("install", () => {
  upgrading = !!self.registration.active;
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  // Delete all caches except current version
  event.waitUntil(
    caches
      .keys()
      .then((names) =>
        Promise.all(
          names
            .filter((name) => name !== CACHE_NAME)
            .map((name) => caches.delete(name))
        )
      )
      .then(() => self.clients.claim())
      // After an upgrade, open tabs reload into the new UI (program spec
      // §5.3); a first visit is never reloaded.
      .then(() => (upgrading ? self.clients.matchAll({ type: "window" }) : []))
      .then((clients) =>
        Promise.all(clients.map((c) => c.navigate(c.url).catch(() => null))),
      ),
  );
});

// Cache-first for content-hashed assets (immutable by definition).
// All other requests (index.html, API, unhashed files) go straight to network.
self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);

  // Only cache same-origin GET requests for hashed files
  if (url.origin !== self.location.origin) return;
  if (event.request.method !== "GET") return;
  if (!HASH_RE.test(url.pathname)) return;

  event.respondWith(
    caches.open(CACHE_NAME).then((cache) =>
      cache.match(event.request).then((cached) => {
        if (cached) return cached;
        return fetch(event.request).then(async (response) => {
          // Await cache.put so the response the page sees is ONLY delivered
          // after the asset has been written to cache. Previously this ran
          // as a detached promise and the page's `networkidle` could fire
          // before the cache was populated, causing post-reload E2E
          // assertions to race against an empty cache.
          if (response.ok) {
            await cache.put(event.request, response.clone());
          }
          return response;
        });
      })
    )
  );
});

// Handle alert notifications from WASM (works when app is in background)
self.addEventListener("message", (event) => {
  if (event.data && event.data.type === "ALERT") {
    const name = event.data.name || "Member";
    self.registration.showNotification(`IEM Alert: ${name}`, {
      body: `${name} needs help!`,
      requireInteraction: true,
      tag: "iem-alert", // Replace previous alert notification
      vibrate: [500, 200, 500, 200, 500],
    });
  }
});

// Clicking notification brings app to foreground
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(
    self.clients.matchAll({ type: "window" }).then((clients) => {
      if (clients.length > 0) {
        return clients[0].focus();
      }
      return self.clients.openWindow("/engineer");
    }),
  );
});

// Handle Web Push notifications (works even when app is fully closed) (reaperiem#133)
self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data?.json() ?? {};
  } catch {
    data = {};
  }

  if (data.type === "ALARM") {
    // The guard's alarms (`iem-server notify`) and the server's own.
    // Each different notice has its own tag (from its text), so one never
    // replaces an unread other; a payload without one keeps the old tag.
    event.waitUntil(
      self.registration.showNotification(data.title || "IEM Mixer", {
        body: data.body || "",
        requireInteraction: true,
        tag: typeof data.tag === "string" && data.tag ? data.tag : "iem-alarm",
        vibrate: [300, 100, 300],
      }),
    );
  } else if (data.type === "SOS") {
    event.waitUntil(
      self.registration.showNotification(`IEM Alert: ${data.name || "Member"}`, {
        body: `${data.name || "Someone"} needs help!`,
        requireInteraction: true,
        tag: "iem-alert",
        vibrate: [500, 200, 500, 200, 500],
      }),
    );
  } else {
    // Generic fallback for unknown push types
    event.waitUntil(
      self.registration.showNotification("IEM Mixer", {
        body: "New alert — tap to open",
        requireInteraction: true,
        tag: "iem-generic",
      }),
    );
  }
});
