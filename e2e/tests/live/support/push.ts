import type { Page, Request } from "@playwright/test";

// The live push spec's support (S7, #10; row 717): a real Web Push
// subscription of the engineer's page in a persistent Chromium profile. A
// push endpoint is a capability (whoever holds it, with the keys, can reach
// the browser), so no error and no console line printed here carries one;
// a URL is never printed either (P6: the host is a site value, a socket URL
// holds a token).

/** The server's push routes the page posts to. */
export const SUBSCRIBE = "/api/push/subscribe";
export const UNSUBSCRIBE = "/api/push/unsubscribe";

/** Whether `request` is a POST to `path` (the path alone, without a query). */
export function isPostTo(request: Pick<Request, "method" | "url">, path: string): boolean {
  if (request.method() !== "POST") return false;
  try {
    return new URL(request.url()).pathname === path;
  } catch {
    return false;
  }
}

/**
 * The push endpoint a subscribe or unsubscribe body names (`{endpoint, …}`):
 * an `https://` URL, or null when the body names none.
 */
export function endpointOf(body: unknown): string | null {
  const endpoint = (body as { endpoint?: unknown } | null)?.endpoint;
  if (typeof endpoint !== "string") return null;
  try {
    return new URL(endpoint).protocol === "https:" ? endpoint : null;
  } catch {
    return null;
  }
}

/** Every URL in `text` (any scheme, up to a space or a quote) as `<url>`. */
export function redacted(text: string): string {
  return text.replace(/\b[a-z][a-z0-9+.-]*:\/\/[^\s"'<>]*/gi, "<url>");
}

/**
 * A console guard of its own for a page outside the fixtures (a persistent
 * context): every error, warning and page error, with no allowance, each
 * with its URLs redacted. The caller asserts the list is empty.
 */
export function guardConsole(page: Page): string[] {
  const problems: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "error" || msg.type() === "warning") problems.push(`[${msg.type()}] ${redacted(msg.text())}`);
  });
  page.on("pageerror", (error) => problems.push(`[pageerror] ${redacted(error.message)}`));
  return problems;
}

/** The endpoint of the page's current push subscription, or null when it holds none. */
export async function browserEndpoint(page: Page): Promise<string | null> {
  return page.evaluate(async () => {
    const registration = await navigator.serviceWorker.ready;
    const subscription = await registration.pushManager.getSubscription();
    return subscription?.endpoint ?? null;
  });
}
