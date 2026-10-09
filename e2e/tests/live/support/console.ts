import type { Page } from "@playwright/test";
import { collectConsole } from "../../support/fixtures";

// The live specs' console lines (S7, #10; P6): a console line can carry a
// URL (the band's host, a socket URL with its token, a push endpoint), and a
// failing guard prints its lines into the report, so every line a live
// guard keeps has its URLs redacted.

/** Every URL in `text` (any scheme, up to a space, a quote or an angle bracket) as `<url>`. */
export function redacted(text: string): string {
  return text.replace(/\b[a-z][a-z0-9+.-]*:\/\/[^\s"'<>]*/gi, "<url>");
}

/**
 * A console guard for a page outside the fixtures (a persistent context):
 * every error, warning and page error, with no allowance, redacted. The
 * caller asserts the list is empty.
 */
export function guardConsole(page: Page): string[] {
  return collectConsole(page, [], redacted);
}
