import { test as base, expect } from "@playwright/test";

type ConsoleGuard = {
  /** Console messages this test deliberately provokes (e.g. a 401 it asks for). */
  allowedConsole: RegExp[];
  consoleGuard: void;
};

/**
 * Every test fails if the browser console shows an error, a warning or a page
 * error that it did not declare in `allowedConsole` (clean-console rule).
 */
export const test = base.extend<ConsoleGuard>({
  allowedConsole: [[], { option: true }],
  consoleGuard: [
    async ({ page, allowedConsole }, use) => {
      // Playwright reads an array whose second element is an object as a
      // `[value, options]` fixture tuple, so `test.use({ allowedConsole:
      // [/a/, /b/] })` would arrive here as the single RegExp /a/. Lists of
      // two or more patterns go in the tuple form
      // `[[/a/, /b/], { scope: "test" }]`.
      if (!Array.isArray(allowedConsole)) {
        throw new Error(
          "allowedConsole must be a RegExp[]: wrap two or more patterns as [[/a/, /b/], { scope: \"test\" }]",
        );
      }
      const problems: string[] = [];
      page.on("console", (msg) => {
        if (msg.type() !== "error" && msg.type() !== "warning") return;
        const text = msg.text();
        if (allowedConsole.some((pattern) => pattern.test(text))) return;
        problems.push(`[${msg.type()}] ${text}`);
      });
      page.on("pageerror", (error) => problems.push(`[pageerror] ${error.message}`));
      await use();
      expect(problems, "browser console must stay clean").toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
export type { Page } from "@playwright/test";

/**
 * REAPER is absent in mock E2E until S5 replaces the REAPER control plane
 * (.claude/rules/e2e.md): pages that load REAPER-era mixer state get exactly
 * these failures. Declare per describe with
 * `test.use({ allowedConsole: REAPER_ABSENT })` and the comment
 * `// REAPER absent in mock E2E until S5`. Every entry is an anchored exact
 * message copied from a CI log line; S5 deletes this list.
 */
export const REAPER_ABSENT: RegExp[] = [
  /^Failed to load resource: the server responded with a status of 502 \(Bad Gateway\)$/,
];
