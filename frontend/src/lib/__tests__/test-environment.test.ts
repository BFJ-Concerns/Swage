import { describe, expect, it } from "vitest";

describe("frontend test environment", () => {
  it("resolves relative API URLs against a deterministic origin", () => {
    expect(new URL("/api/settings", window.location.href).href).toBe(
      "http://localhost/api/settings",
    );
  });
});
