/**
 * E2E PINs come from the environment: CI generates them per run and
 * provisions them with `iem-server pin …`. No credential is committed.
 */
function requiredPin(name: string): string {
  const value = process.env[name];
  if (!value || !/^\d{4}$/.test(value)) {
    throw new Error(`${name} must be a 4-digit PIN (set by the e2e job in .github/workflows/ci.yml)`);
  }
  return value;
}

export const ENGINEER_PIN = requiredPin("E2E_ENGINEER_PIN");
export const MEMBER_PIN = requiredPin("E2E_MEMBER_PIN");

/** A 4-digit PIN that is neither the member PIN nor the engineer PIN. */
export function wrongPin(): string {
  for (let offset = 1; offset < 10; offset++) {
    const candidate = String((Number(MEMBER_PIN) + offset) % 10000).padStart(4, "0");
    if (candidate !== ENGINEER_PIN) return candidate;
  }
  throw new Error("two PINs cannot block nine candidates");
}
