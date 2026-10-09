import type { Page, Request } from "@playwright/test";

// The live push spec's support (S7, #10; row 717): a real Web Push
// subscription of the engineer's page in a persistent Chromium profile. A
// push endpoint is a capability (whoever holds it, with the keys, can reach
// the browser), so no error here carries one, nor a URL (P6: the host is a
// site value). A request's body is read without `postDataJSON()`, whose
// error text holds the whole body.

/** The server's push routes the page posts to. */
export const SUBSCRIBE = "/api/push/subscribe";
export const UNSUBSCRIBE = "/api/push/unsubscribe";

/** How long the page's service worker may take to be ready. */
const READY_MS = 10_000;

type Sent = Pick<Request, "method" | "url" | "postData">;

/** Whether `request` is a POST to `path` (the path alone, without a query). */
export function isPostTo(request: Pick<Request, "method" | "url">, path: string): boolean {
  if (request.method() !== "POST") return false;
  try {
    return new URL(request.url()).pathname === path;
  } catch {
    return false;
  }
}

/** A request's JSON body, or null when it has none or it is not JSON (never throws). */
export function bodyOf(request: Pick<Request, "postData">): unknown {
  try {
    return JSON.parse(request.postData() ?? "null");
  } catch {
    return null;
  }
}

/** The endpoint a subscribe or unsubscribe body names, any non-empty string (what the server stores), or null. */
export function postedEndpoint(body: unknown): string | null {
  const endpoint = (body as { endpoint?: unknown } | null)?.endpoint;
  return typeof endpoint === "string" && endpoint !== "" ? endpoint : null;
}

/** The endpoint a body names when it is an `https://` URL, as a browser's push service gives it; else null. */
export function endpointOf(body: unknown): string | null {
  const endpoint = postedEndpoint(body);
  if (endpoint === null) return null;
  try {
    return new URL(endpoint).protocol === "https:" ? endpoint : null;
  } catch {
    return null;
  }
}

/**
 * The endpoints a profile posted to the server and has not had revoked: a
 * subscribe adds its endpoint when it is sent, an unsubscribe that answered
 * 200 removes its own. What is left at the end is still on the server.
 */
export class PushLedger {
  private readonly posted = new Set<string>();

  /** A request the profile sent. */
  sent(request: Sent): void {
    if (!isPostTo(request, SUBSCRIBE)) return;
    const endpoint = postedEndpoint(bodyOf(request));
    if (endpoint !== null) this.posted.add(endpoint);
  }

  /** A response the profile got. */
  answered(response: { status(): number; request(): Sent }): void {
    if (response.status() !== 200 || !isPostTo(response.request(), UNSUBSCRIBE)) return;
    const endpoint = postedEndpoint(bodyOf(response.request()));
    if (endpoint !== null) this.posted.delete(endpoint);
  }

  /** The endpoints still on the server, in the order they were posted. */
  pending(): string[] {
    return [...this.posted];
  }
}

/**
 * The page's push subscription: its endpoint, or null when it holds none;
 * with `unsubscribe`, it is also unsubscribed in the browser. Fails with
 * fixed words when the service worker is not ready within 10 s.
 */
export async function browserEndpoint(page: Page, unsubscribe = false): Promise<string | null> {
  const found = await page.evaluate(
    async ({ ms, drop }) => {
      const ready = await Promise.race([
        navigator.serviceWorker.ready,
        new Promise<null>((resolve) => setTimeout(() => resolve(null), ms)),
      ]);
      if (ready === null) return { ready: false, endpoint: null };
      const subscription = await ready.pushManager.getSubscription();
      if (subscription !== null && drop) await subscription.unsubscribe();
      return { ready: true, endpoint: subscription?.endpoint ?? null };
    },
    { ms: READY_MS, drop: unsubscribe },
  );
  if (!found.ready) throw new Error(`the page's service worker was not ready within ${READY_MS / 1000} s`);
  return found.endpoint;
}
