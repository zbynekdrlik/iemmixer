import { readFileSync } from "node:fs";
import { expect, type APIRequestContext, type APIResponse, type Page } from "@playwright/test";

// The live run's site values (S7, #10; P6): every one comes from a LIVE_*
// variable of the ops live run, read on first use, so `--list` works without
// any of them. A refusal names the variable, never its value, and no error
// here carries a URL (a socket URL holds a token in its query).

export type Who = "engineer" | "member";

export type Live = {
  /** The band's public host, `https://…` (the members' real path, through the tunnel). */
  baseURL: string;
  /** The run's build: the 40-hex SHA the PC runs. */
  sha: string;
  /** Short-lived tokens the PC minted for this run (`iem-soakclient token`). */
  tokens: { engineer: string; member: string };
  /** The member whose page a member-side spec opens. */
  member: string;
  /** The input the bursts drive, and the talkback's input. */
  testInput: string;
  talkbackInput: string;
  /** The bursts' level (dBFS, at most the HIL ceiling of −20). */
  burstDbfs: number;
};

const ID = /^[A-Za-z0-9_-]{1,64}$/;
const SHA = /^[0-9a-f]{40}$/;
const JWT = /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/;
const DECIMAL = /^-?[0-9]+(?:\.[0-9]+)?$/;
/** The HIL ceiling (`iem-guard` daemon): a burst is never louder. */
export const BURST_CEILING_DBFS = -20;

/** The failing hop when /api/version does not name the run's build. */
export const NOT_THE_BUILD = "the public host does not answer with the run's build (tunnel or server)";

/** The variables a value is read from: the process's, or a test's own. */
type Env = Record<string, string | undefined>;

function value(name: string, env: Env = process.env): string {
  const v = env[name];
  if (v === undefined || v === "") throw new Error(`${name} is not set`);
  return v;
}

function matching(name: string, re: RegExp, what: string, env: Env = process.env): string {
  const v = value(name, env);
  if (!re.test(v)) throw new Error(`${name} is not ${what}`);
  return v;
}

function baseURLOf(name: string): string {
  const v = matching(name, /^https:\/\//, "an https:// URL");
  let url: URL;
  try {
    url = new URL(v);
  } catch {
    throw new Error(`${name} is not an https:// URL`);
  }
  if (url.pathname !== "/" || url.search !== "" || url.hash !== "" || url.username !== "" || url.password !== "") {
    throw new Error(`${name} must be a bare origin (no path, query, fragment or credentials)`);
  }
  return url.origin;
}

function tokensOf(name: string): Live["tokens"] {
  const path = value(name);
  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(path, "utf8"));
  } catch {
    throw new Error(`${name} does not name a readable JSON file`);
  }
  const t = parsed as { engineer?: unknown; member?: unknown } | null;
  const ok = (v: unknown): v is string => typeof v === "string" && JWT.test(v);
  if (typeof t !== "object" || t === null || !ok(t.engineer) || !ok(t.member)) {
    throw new Error(`${name} does not hold {engineer, member} tokens`);
  }
  return { engineer: t.engineer, member: t.member };
}

function burstOf(name: string): number {
  const v = matching(name, DECIMAL, "a decimal number");
  const n = Number(v);
  if (!Number.isFinite(n) || n > BURST_CEILING_DBFS) throw new Error(`${name} is not a level of at most ${BURST_CEILING_DBFS} dBFS`);
  return n;
}

/** A run id or an attempt of GitHub Actions: decimal digits, at most 20 (a u64). */
const RUN_NUMBER = /^[0-9]{1,20}$/;

/**
 * The client-error marker of this run, `iemmixer-live-marker-<run id>-<attempt>`,
 * from `GITHUB_RUN_ID` and `GITHUB_RUN_ATTEMPT` (the ops live run's own:
 * `pc-end` in the same run builds the same marker and looks for it in the
 * server's log). Read on use; a refusal names the variable, never its value.
 */
export function runMarker(env: Env = process.env): string {
  const id = matching("GITHUB_RUN_ID", RUN_NUMBER, "a run number (digits)", env);
  const attempt = matching("GITHUB_RUN_ATTEMPT", RUN_NUMBER, "a run number (digits)", env);
  return `iemmixer-live-marker-${id}-${attempt}`;
}

let cached: Live | null = null;

/** The live run's values, read and checked on the first call. */
export function live(): Live {
  cached ??= {
    baseURL: baseURLOf("LIVE_BASE_URL"),
    sha: matching("LIVE_SHA", SHA, "a 40-hex SHA"),
    tokens: tokensOf("LIVE_TOKENS"),
    member: matching("LIVE_MEMBER", ID, "an id"),
    testInput: matching("LIVE_TEST_INPUT", ID, "an id"),
    talkbackInput: matching("LIVE_TALKBACK_INPUT", ID, "an id"),
    burstDbfs: burstOf("LIVE_BURST_DBFS"),
  };
  return cached;
}

/**
 * Whether `/api/version`'s `git_hash` names `sha`: at least 7 lowercase hex
 * digits and a prefix of it (the rule of `Test-IemHilVersion`, the guard's
 * `version_matches` and the soak client's `names_build`).
 */
export function namesBuild(gitHash: unknown, sha: string): boolean {
  return typeof gitHash === "string" && /^[0-9a-f]{7,}$/.test(gitHash) && sha.startsWith(gitHash);
}

/**
 * Fails unless the public host answers `/api/version` with the run's build:
 * after "ide event" the predecessor answers at the same address, so every
 * real socket opens only after this check.
 */
export async function expectBuild(request: APIRequestContext): Promise<void> {
  const { baseURL, sha } = live();
  let answer = "no answer";
  let gitHash: unknown;
  try {
    const r = await request.get(new URL("/api/version", baseURL).toString(), {
      timeout: 15_000,
      maxRedirects: 0,
      failOnStatusCode: false,
    });
    answer = `HTTP ${r.status()}`;
    if (r.ok()) gitHash = ((await r.json()) as { git_hash?: unknown } | null)?.git_hash;
  } catch {
    // The request's own error names the URL; the hop and the status are enough.
  }
  if (!namesBuild(gitHash, sha)) throw new Error(`${NOT_THE_BUILD}: ${answer}`);
}

/** A stored login, as the UI keeps it (`iem_token`). */
export type Auth = { token: string; member: string; engineer: boolean };

/** The stored login of `who`. */
function authOf(who: Who): Auth {
  const l = live();
  return who === "engineer"
    ? { token: l.tokens.engineer, member: "engineer", engineer: true }
    : { token: l.tokens.member, member: l.member, engineer: false };
}

/** Stores `auth` on `origin` as the UI keeps a login, from a page of that origin. */
export async function storeLogin(page: Page, origin: string, auth: Auth): Promise<void> {
  await navigate(page, new URL("/", origin).toString(), "the app's start page");
  await page.evaluate((a) => {
    localStorage.setItem("iem_token", JSON.stringify(a));
    sessionStorage.setItem("iem_redirected", "1");
  }, auth);
}

/**
 * Opens the page `path` (default: `who`'s own) signed in with the run's
 * token, as `openMixer` does without a login; returns once the page's state
 * has arrived (IEM VOL is shown).
 */
export async function openLive(page: Page, who: Who, path?: string): Promise<void> {
  const auth = authOf(who);
  const { baseURL } = live();
  await storeLogin(page, baseURL, auth);
  const what = who === "engineer" ? "a mixer page" : "the member's mixer page";
  await navigate(page, new URL(`/${path ?? auth.member}`, baseURL).toString(), what);
  await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
}

/** Opens the public host's start page (the band's landing page). */
export async function openStart(page: Page): Promise<void> {
  await navigate(page, new URL("/", live().baseURL).toString(), "the app's start page");
}

/**
 * `page.goto(url)`; a failure names `what` and Chromium's net error code,
 * never the URL (Playwright's own text holds the host and the page, P6).
 */
async function navigate(page: Page, url: string, what: string): Promise<void> {
  try {
    await page.goto(url);
  } catch (e) {
    const net = /net::ERR_[A-Z_]+/.exec(String(e))?.[0];
    const code = net ?? ((e as Error).name === "TimeoutError" ? "a timeout" : "no answer");
    throw new Error(`the public host did not serve ${what}: ${code}`);
  }
}

/**
 * GET `path` (no query) from the public host with `who`'s token: its status
 * and JSON body (null when it has none). A failure names the path only:
 * Playwright's own error text lists the request's headers, the token too.
 */
export async function apiGet(request: APIRequestContext, who: Who, path: string): Promise<{ status: number; body: unknown }> {
  return api(request, "GET", who, path);
}

/**
 * POST `data` as JSON to `path` (no query) on the public host with `who`'s
 * token: its status and JSON body, as `apiGet`. A failure names the method
 * and the path only, never the data (a push endpoint is a capability) or the
 * token.
 */
export async function apiPost(
  request: APIRequestContext,
  who: Who,
  path: string,
  data: unknown,
): Promise<{ status: number; body: unknown }> {
  return api(request, "POST", who, path, data);
}

async function api(
  request: APIRequestContext,
  method: "GET" | "POST",
  who: Who,
  path: string,
  data?: unknown,
): Promise<{ status: number; body: unknown }> {
  if (!/^\/[^?#]*$/.test(path)) throw new Error(`${method} takes a path without a query`);
  return apiAt(request, { origin: live().baseURL, token: authOf(who).token }, method, path, data);
}

/**
 * `method` `path` at `at.origin` with `at.token`; the request behind
 * `apiGet` / `apiPost`, given its origin and token (the mock tests' seam).
 * A request that gets no answer fails with the method and the path only:
 * Playwright's own error text lists the URL and the request's headers.
 */
export async function apiAt(
  request: Pick<APIRequestContext, "fetch">,
  at: { origin: string; token: string },
  method: "GET" | "POST",
  path: string,
  data?: unknown,
): Promise<{ status: number; body: unknown }> {
  let response: APIResponse;
  try {
    response = await request.fetch(new URL(path, at.origin).toString(), {
      method,
      headers: { Authorization: `Bearer ${at.token}` },
      data,
      timeout: 15_000,
      maxRedirects: 0,
      failOnStatusCode: false,
    });
  } catch {
    throw new Error(`${method} ${path} got no answer`);
  }
  let body: unknown = null;
  try {
    body = await response.json();
  } catch {
    body = null;
  }
  return { status: response.status(), body };
}
