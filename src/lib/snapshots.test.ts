import { describe, expect, it } from "vitest";
import type { ProviderSnapshot } from "../types";
import { mergeSnapshots } from "./snapshots";

const success: ProviderSnapshot = {
  provider: "codex",
  displayName: "CODEX",
  plan: "PRO",
  quotaHistoryScope: "scope-a",
  shortWindow: { remainingPercent: 74, resetsAt: "2026-07-07T02:00:00Z", windowSeconds: 18_000 },
  weeklyWindow: { remainingPercent: 42, resetsAt: "2026-07-10T00:00:00Z", windowSeconds: 604_800 },
  resetCredits: 1,
  updatedAt: "2026-07-07T00:00:00Z",
  status: "ok",
  message: null,
};

describe("snapshot failure handling", () => {
  it("retains the last successful values during a transient failure", () => {
    const failure: ProviderSnapshot = { ...success, shortWindow: null, weeklyWindow: null, status: "unavailable", message: "Network unavailable", updatedAt: "2026-07-07T01:00:00Z" };
    expect(mergeSnapshots([success], [failure])[0]).toEqual({ ...success, status: "stale", message: "Network unavailable" });
  });

  it("shows a failure when no successful snapshot exists", () => {
    const signedOut: ProviderSnapshot = { ...success, quotaHistoryScope: null, shortWindow: null, weeklyWindow: null, status: "signed_out", message: "Please sign in" };
    expect(mergeSnapshots([], [signedOut])[0].status).toBe("signed_out");
  });

  it("does not hide an expired login behind stale quota data", () => {
    const signedOut: ProviderSnapshot = { ...success, quotaHistoryScope: null, shortWindow: null, weeklyWindow: null, status: "signed_out", message: "Please sign in" };
    expect(mergeSnapshots([success], [signedOut])[0].status).toBe("signed_out");
  });

  it("replaces stale data after recovery", () => {
    expect(mergeSnapshots([{ ...success, status: "stale" }], [{ ...success, shortWindow: { ...success.shortWindow!, remainingPercent: 88 } }])[0].shortWindow?.remainingPercent).toBe(88);
  });

  it("preserves the last known plan when a successful refresh omits it", () => {
    const refresh = { ...success, plan: null };
    expect(mergeSnapshots([success], [refresh])[0].plan).toBe("PRO");
  });

  it("does not inherit another account's plan or quota", () => {
    const refresh = { ...success, plan: null, quotaHistoryScope: "scope-b", shortWindow: null, weeklyWindow: null };
    expect(mergeSnapshots([success], [refresh])[0]).toEqual(refresh);
  });

  it("preserves the plan for weekly-only snapshots during a transient failure", () => {
    const weeklyOnly = { ...success, shortWindow: null };
    const failure = { ...weeklyOnly, weeklyWindow: null, status: "unavailable" as const, message: "Network unavailable" };
    expect(mergeSnapshots([weeklyOnly], [failure])[0]).toEqual({ ...weeklyOnly, status: "stale", message: "Network unavailable" });
  });

  it("does not turn an unidentified-account failure into stale data", () => {
    const failure = { ...success, quotaHistoryScope: null, shortWindow: null, weeklyWindow: null, status: "unavailable" as const, message: "Network unavailable" };
    expect(mergeSnapshots([success], [failure])[0]).toEqual(failure);
  });
});
