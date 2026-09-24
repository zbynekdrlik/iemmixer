import { ENGINEER_PIN, MEMBER_PIN } from "./support/pins";
/**
 * Member Photo Tests — upload, display, and remove profile photos (#16).
 */

import { test, expect } from "./support/fixtures";

const BASE_URL = process.env.E2E_BASE_URL || "http://localhost:80";

// Minimal valid 1x1 white JPEG (base64-encoded, 631 bytes)
const TINY_JPEG =
  "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoH" +
  "BwYIDAoMCwsKCwsNCRASDQ4REQ0MEhMSFxoWGBYbFhkeFxkdHR0dHR3/2wBDAQME" +
  "BAUEBQkFBQkdDQsNHR0dHR0dHR0dHR0dHR0dHR0dHR0dHR0dHR0dHR0dHR0dHR0d" +
  "HR0dHR0dHR0dHR3/wAARCAABAAEDASIAAhEBAxEB/8QAFAABAAAAAAAAAAAAAAAAAAAACf" +
  "/EABQQAQAAAAAAAAAAAAAAAAAAAAD/xAAUAQEAAAAAAAAAAAAAAAAAAAAA/8QAFBEBAAAA" +
  "AAAAAAAAAAAAAAAAAP/aAAwDAQACEQMRAD8AVMAA";

test.describe("Member Photos — Issue #16", () => {
  test("GET /api/members includes has_photo field", async ({ request }) => {
    const response = await request.get(`${BASE_URL}/api/members`);
    expect(response.status()).toBe(200);
    const members = await response.json();
    expect(members.length).toBeGreaterThan(0);
    expect(members[0]).toHaveProperty("has_photo");
    expect(typeof members[0].has_photo).toBe("boolean");
  });

  test("GET photo returns 404 when no photo set", async ({ request }) => {
    const membersResp = await request.get(`${BASE_URL}/api/members`);
    const members = await membersResp.json();
    const member = members[0];

    // Ensure no photo exists (cleanup from prior test runs)
    const loginResp = await request.post(`${BASE_URL}/api/auth`, {
      data: { member: member.id, pin: ENGINEER_PIN },
    });
    if (loginResp.ok()) {
      const { token } = await loginResp.json();
      await request.delete(`${BASE_URL}/api/members/${member.id}/photo`, {
        headers: { Authorization: `Bearer ${token}` },
      });
    }

    const response = await request.get(
      `${BASE_URL}/api/members/${member.id}/photo`,
    );
    expect(response.status()).toBe(404);
  });

  test("POST photo requires auth", async ({ request }) => {
    const membersResp = await request.get(`${BASE_URL}/api/members`);
    const members = await membersResp.json();
    const member = members[0];

    const response = await request.post(
      `${BASE_URL}/api/members/${member.id}/photo`,
      {
        data: { photo: TINY_JPEG },
      },
    );
    expect(response.status()).toBe(401);
  });

  test("upload, retrieve, and delete photo round-trip", async ({
    request,
  }) => {
    const membersResp = await request.get(`${BASE_URL}/api/members`);
    const members = await membersResp.json();
    const member = members[0];

    // Login as engineer (can access any member)
    const loginResp = await request.post(`${BASE_URL}/api/auth`, {
      data: { member: member.id, pin: ENGINEER_PIN },
    });
    expect(loginResp.status()).toBe(200);
    const { token } = await loginResp.json();

    // Upload
    const uploadResp = await request.post(
      `${BASE_URL}/api/members/${member.id}/photo`,
      {
        headers: { Authorization: `Bearer ${token}` },
        data: { photo: TINY_JPEG },
      },
    );
    expect(uploadResp.status()).toBe(200);

    // Verify has_photo is now true
    const afterUpload = await request.get(`${BASE_URL}/api/members`);
    const membersAfter = await afterUpload.json();
    const updated = membersAfter.find((m: any) => m.id === member.id);
    expect(updated?.has_photo).toBe(true);

    // GET photo returns image/jpeg
    const photoResp = await request.get(
      `${BASE_URL}/api/members/${member.id}/photo`,
    );
    expect(photoResp.status()).toBe(200);
    expect(photoResp.headers()["content-type"]).toContain("image/jpeg");

    // DELETE photo
    const deleteResp = await request.delete(
      `${BASE_URL}/api/members/${member.id}/photo`,
      {
        headers: { Authorization: `Bearer ${token}` },
      },
    );
    expect(deleteResp.status()).toBe(200);

    // Verify gone
    const afterDelete = await request.get(
      `${BASE_URL}/api/members/${member.id}/photo`,
    );
    expect(afterDelete.status()).toBe(404);

    // Verify has_photo is false again
    const final_members = await request.get(`${BASE_URL}/api/members`);
    const finalList = await final_members.json();
    const finalMember = finalList.find((m: any) => m.id === member.id);
    expect(finalMember?.has_photo).toBe(false);
  });

  test("rejects oversized photo (> 256 KB decoded)", async ({ request }) => {
    const membersResp = await request.get(`${BASE_URL}/api/members`);
    const members = await membersResp.json();
    const member = members[0];

    const loginResp = await request.post(`${BASE_URL}/api/auth`, {
      data: { member: member.id, pin: ENGINEER_PIN },
    });
    const { token } = await loginResp.json();

    // 300 KB of zeros, base64-encoded
    const bigData = Buffer.alloc(300 * 1024).toString("base64");
    const resp = await request.post(
      `${BASE_URL}/api/members/${member.id}/photo`,
      {
        headers: { Authorization: `Bearer ${token}` },
        data: { photo: bigData },
      },
    );
    expect(resp.status()).toBe(400);
  });

  test("member cannot upload photo for another member", async ({
    request,
  }) => {
    const membersResp = await request.get(`${BASE_URL}/api/members`);
    const members = await membersResp.json();
    if (members.length < 2) return; // Need at least 2 members

    const member = members[0];
    const other = members[1];

    // Login as member (not engineer)
    const loginResp = await request.post(`${BASE_URL}/api/auth`, {
      data: { member: member.id, pin: MEMBER_PIN },
    });
    expect(loginResp.status()).toBe(200);
    const { token } = await loginResp.json();

    // Try to upload for a different member
    const resp = await request.post(
      `${BASE_URL}/api/members/${other.id}/photo`,
      {
        headers: { Authorization: `Bearer ${token}` },
        data: { photo: TINY_JPEG },
      },
    );
    expect(resp.status()).toBe(403);
  });

  test("photo is served as image/jpeg after upload", async ({ request }) => {
    const membersResp = await request.get(`${BASE_URL}/api/members`);
    const members = await membersResp.json();
    const member = members[0];

    // Login as engineer
    const loginResp = await request.post(`${BASE_URL}/api/auth`, {
      data: { member: member.id, pin: ENGINEER_PIN },
    });
    const { token } = await loginResp.json();

    // Upload photo
    await request.post(`${BASE_URL}/api/members/${member.id}/photo`, {
      headers: { Authorization: `Bearer ${token}` },
      data: { photo: TINY_JPEG },
    });

    // Verify photo is served correctly
    const photoResp = await request.get(
      `${BASE_URL}/api/members/${member.id}/photo`,
    );
    expect(photoResp.status()).toBe(200);
    expect(photoResp.headers()["content-type"]).toContain("image/jpeg");
    expect(photoResp.headers()["cache-control"]).toContain("public");

    // Verify has_photo in members list
    const afterResp = await request.get(`${BASE_URL}/api/members`);
    const afterMembers = await afterResp.json();
    const updated = afterMembers.find((m: any) => m.id === member.id);
    expect(updated?.has_photo).toBe(true);

    // Cleanup
    await request.delete(`${BASE_URL}/api/members/${member.id}/photo`, {
      headers: { Authorization: `Bearer ${token}` },
    });
  });
});
