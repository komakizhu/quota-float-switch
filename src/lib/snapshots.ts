import type { ProviderSnapshot } from "../types";

export function mergeSnapshots(current: ProviderSnapshot[], incoming: ProviderSnapshot[]): ProviderSnapshot[] {
  return incoming.map((next) => {
    const previous = current.find((item) => item.provider === next.provider);
    const sameHistoryScope = previous?.quotaHistoryScope !== null
      && previous?.quotaHistoryScope !== undefined
      && next.quotaHistoryScope !== null
      && next.quotaHistoryScope !== undefined
      && previous.quotaHistoryScope === next.quotaHistoryScope;
    // Some usage responses omit the plan while refreshing an otherwise valid
    // snapshot. Keep the last known plan so a skin does not silently fall back
    // to its generic PLUS label for one refresh cycle.
    if (next.status === "ok") return sameHistoryScope && previous?.plan && !next.plan ? { ...next, plan: previous.plan } : next;
    if (next.status === "signed_out") return next;
    const previousWithQuota = sameHistoryScope && previous && (previous.shortWindow || previous.weeklyWindow) ? previous : null;
    return previousWithQuota
      ? { ...previousWithQuota, plan: next.plan ?? previousWithQuota.plan, status: "stale", message: next.message, updatedAt: previousWithQuota.updatedAt }
      : next;
  });
}
