import { test, expect } from "./support/live";
import { expectBuild, live, openStart, runMarker } from "./support/env";

// The client error report on the real PC (S7, #10; row 555, F25): the start
// page posts a panic report as the UI's panic hook does, from the page and
// through the band's public host, with this run's marker
// `iemmixer-live-marker-<GITHUB_RUN_ID>-<GITHUB_RUN_ATTEMPT>` as its
// message. The server answers 204 and logs it; `pc-end` of the same run
// builds the same marker and finds it in the server's log on the PC
// (`<root>\logs\server.log`, not the tray's rolling log). The marker is
// read before the page opens, so a run without it fails naming the
// variable. Any page socket goes through the runner's relay (the fixture).

test.describe("the client error report on the real PC (S7)", () => {
  test("the marker panic report is accepted on the real PC", async ({ page, relay }) => {
    const marker = runMarker();
    const report = {
      panic_message: marker,
      git_hash: live().sha.slice(0, 7),
      url: "/",
      location: "e2e/tests/live/client-error.spec.ts",
    };
    await openStart(page);
    // Only to the run's build: after "ide event" the predecessor answers at the same address.
    await expectBuild(page.request);
    // From the page, as the panic hook posts: same origin, through the tunnel.
    // A failed fetch comes back as 0 (its error text could name the URL).
    const status = await page.evaluate(async (body) => {
      try {
        const r = await fetch("/api/client-error", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ...body, user_agent: navigator.userAgent }),
        });
        return r.status;
      } catch {
        return 0;
      }
    }, report);
    expect(status, "POST /api/client-error (0: no answer)").toBe(204);
    relay.check();
  });
});
