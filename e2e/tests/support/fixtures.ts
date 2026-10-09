import { test as base, expect, type Page } from "@playwright/test";

type ConsoleGuard = {
  /** Console messages this test deliberately provokes (e.g. a 401 it asks for). */
  allowedConsole: RegExp[];
  consoleGuard: void;
};

/**
 * Collects every console error, warning and page error of `page` that
 * `allowed` does not declare (matched on the line as the browser gave it),
 * each shown through `shown` (the live specs redact URLs). The caller
 * asserts the list is empty.
 */
export function collectConsole(page: Page, allowed: RegExp[], shown: (text: string) => string = (t) => t): string[] {
  // Playwright reads an array whose second element is an object as a
  // `[value, options]` fixture tuple, so `test.use({ allowedConsole:
  // [/a/, /b/] })` would arrive here as the single RegExp /a/. Lists of
  // two or more patterns go in the tuple form
  // `[[/a/, /b/], { scope: "test" }]`.
  if (!Array.isArray(allowed)) {
    throw new Error(
      "allowedConsole must be a RegExp[]: wrap two or more patterns as [[/a/, /b/], { scope: \"test\" }]",
    );
  }
  const problems: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() !== "error" && msg.type() !== "warning") return;
    const text = msg.text();
    if (allowed.some((pattern) => pattern.test(text))) return;
    problems.push(`[${msg.type()}] ${shown(text)}`);
  });
  page.on("pageerror", (error) => problems.push(`[pageerror] ${shown(error.message)}`));
  return problems;
}

/**
 * Every test fails if the browser console shows an error, a warning or a page
 * error that it did not declare in `allowedConsole` (clean-console rule).
 */
export const test = base.extend<ConsoleGuard>({
  allowedConsole: [[], { option: true }],
  consoleGuard: [
    async ({ page, allowedConsole }, use) => {
      const problems = collectConsole(page, allowedConsole);
      await use();
      expect(problems, "browser console must stay clean").toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
export type { Page } from "@playwright/test";
