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

function value(name: string): string {
  const v = process.env[name];
  if (v === undefined || v === "") throw new Error(`${name} is not set`);
  return v;
}

function matching(name: string, re: RegExp, what: string): string {
  const v = value(name);
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

/** The stored login of `who`, as the UI keeps it (`iem_token`). */
function authOf(who: Who): { token: string; member: string; engineer: boolean } {
  const l = live();
  return who === "engineer"
    ? { token: l.tokens.engineer, member: "engineer", engineer: true }
    : { token: l.tokens.member, member: l.member, engineer: false };
}

/**
 * Opens the page `path` (default: `who`'s own) signed in with the run's
 * token, as `openMixer` does without a login; returns once the page's state
 * has arrived (IEM VOL is shown).
 */
export async function openLive(page: Page, who: Who, path?: string): Promise<void> {
  const auth = authOf(who);
  const { baseURL } = live();
  await page.goto(new URL("/", baseURL).toString());
  await page.evaluate((a) => {
    localStorage.setItem("iem_token", JSON.stringify(a));
    sessionStorage.setItem("iem_redirected", "1");
  }, auth);
  await page.goto(new URL(`/${path ?? auth.member}`, baseURL).toString());
  await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
}

/**
 * GET `path` (no query) from the public host with `who`'s token: its status
 * and JSON body (null when it has none). A failure names the path only:
 * Playwright's own error text lists the request's headers, the token too.
 */
export async function apiGet(request: APIRequestContext, who: Who, path: string): Promise<{ status: number; body: unknown }> {
  if (!/^\/[^?#]*$/.test(path)) throw new Error("apiGet takes a path without a query");
  const { baseURL } = live();
  let response: APIResponse;
  try {
    response = await request.get(new URL(path, baseURL).toString(), {
      headers: { Authorization: `Bearer ${authOf(who).token}` },
      timeout: 15_000,
      maxRedirects: 0,
      failOnStatusCode: false,
    });
  } catch {
    throw new Error(`GET ${path} got no answer`);
  }
  let body: unknown = null;
  try {
    body = await response.json();
  } catch {
    body = null;
  }
  return { status: response.status(), body };
}
