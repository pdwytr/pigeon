import { describe, expect, it } from "vitest";
import { type SessionKey, sameSession, sessionKeyId } from "./bindings";

describe("session identity", () => {
  const claude: SessionKey = { providerId: "claude-code", sid: "0199c4a1-2b3d-7e4f-8a9b-0c1d" };
  const codex: SessionKey = { providerId: "codex", sid: "0199c4a1-2b3d-7e4f-8a9b-0c1d" };

  it("namespaces the id by provider so two engines sharing a sid stay distinct", () => {
    expect(sessionKeyId(claude)).not.toBe(sessionKeyId(codex));
    expect(sameSession(claude, codex)).toBe(false);
  });

  it("uses the complete sid, never a prefix", () => {
    // UUIDv7 prefixes collide every ~65 s — studio's ADR-0046 found 8 colliding pairs in 110.
    expect(sessionKeyId(claude)).toContain(claude.sid);
  });

  it("treats a null selection as no match rather than a match against itself", () => {
    expect(sameSession(null, null)).toBe(false);
    expect(sameSession(claude, null)).toBe(false);
  });
});
