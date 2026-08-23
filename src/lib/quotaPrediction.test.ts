import { describe, expect, it } from "vitest";
import { assignLegacyQuotaHistoryToScope, calculateQuotaPrediction, loadQuotaHistory, recordQuotaSample, quotaHistoryKey, saveQuotaHistory, type QuotaHistory } from "./quotaPrediction";

describe("quota prediction", () => {
  const now = new Date("2026-08-17T12:00:00Z");
  const snapshot = { provider: "codex" as const, quotaHistoryScope: "scope-a" };
  const window = { windowSeconds: 604800 };

  it("keeps one local sample per day and trims old samples", () => {
    let history: QuotaHistory = {};
    const key = quotaHistoryKey(snapshot, window)!;
    for (let day = 1; day <= 10; day += 1) {
      history = recordQuotaSample(history, key, 100 - day, new Date(`2026-08-${String(day).padStart(2, "0")}T12:00:00Z`));
    }
    expect(history[key]).toHaveLength(8);
    expect(history[key][0].day).toBe("2026-08-03");
    history = recordQuotaSample(history, key, 12, new Date("2026-08-10T08:00:00Z"));
    expect(history[key]).toHaveLength(8);
    expect(history[key].find((point) => point.day === "2026-08-10")?.remainingPercent).toBe(12);
  });

  it("calculates average daily use, runway, and daily budget", () => {
    const points = [
      { day: "2026-08-14", remainingPercent: 80 },
      { day: "2026-08-15", remainingPercent: 70 },
      { day: "2026-08-16", remainingPercent: 60 },
    ];
    const prediction = calculateQuotaPrediction(50, "2026-08-20T12:00:00Z", points, now);
    expect(prediction.historyDays).toBe(4);
    expect(prediction.averageDailyUsagePercent).toBe(10);
    expect(prediction.daysAtAverage).toBe(5);
    expect(prediction.daysUntilReset).toBe(3);
    expect(prediction.recommendedDailyPercent).toBeCloseTo(16.6667, 4);
  });

  it("does not use the reset horizon before a consumption interval exists", () => {
    const prediction = calculateQuotaPrediction(96, "2026-08-24T12:00:00Z", [], now);

    expect(prediction.historyDays).toBe(1);
    expect(prediction.averageDailyUsagePercent).toBeNull();
    expect(prediction.daysAtAverage).toBeNull();
    expect(prediction.recommendedDailyPercent).toBeNull();
    expect(prediction.daysUntilReset).toBe(7);
  });

  it("uses one day of consumption when yesterday is the only history point", () => {
    const points = [{ day: "2026-08-16", remainingPercent: 80 }];
    const prediction = calculateQuotaPrediction(70, "2026-08-24T12:00:00Z", points, now);

    expect(prediction.historyDays).toBe(2);
    expect(prediction.averageDailyUsagePercent).toBe(10);
    expect(prediction.daysAtAverage).toBe(7);
    expect(prediction.recommendedDailyPercent).toBeCloseTo(10, 4);
  });

  it("averages the actual covered days for two and seven day histories", () => {
    const twoDay = calculateQuotaPrediction(70, null, [
      { day: "2026-08-15", remainingPercent: 90 },
      { day: "2026-08-16", remainingPercent: 80 },
    ], now);
    expect(twoDay.averageDailyUsagePercent).toBe(10);
    expect(twoDay.daysAtAverage).toBe(7);

    const sevenDay = calculateQuotaPrediction(30, null, [
      { day: "2026-08-10", remainingPercent: 65 },
      { day: "2026-08-11", remainingPercent: 60 },
      { day: "2026-08-12", remainingPercent: 55 },
      { day: "2026-08-13", remainingPercent: 50 },
      { day: "2026-08-14", remainingPercent: 45 },
      { day: "2026-08-15", remainingPercent: 40 },
      { day: "2026-08-16", remainingPercent: 35 },
    ], now);
    expect(sevenDay.historyDays).toBe(8);
    expect(sevenDay.averageDailyUsagePercent).toBe(5);
    expect(sevenDay.daysAtAverage).toBe(6);
  });

  it("returns dashes-worthy null values when all recent points are invalid", () => {
    const prediction = calculateQuotaPrediction(85, "2026-08-20T12:00:00Z", [
      { day: "2026-08-16", remainingPercent: 20 },
      { day: "2026-08-17", remainingPercent: 95 },
    ], now);

    expect(prediction.averageDailyUsagePercent).toBeNull();
    expect(prediction.daysAtAverage).toBeNull();
    expect(prediction.recommendedDailyPercent).toBeNull();
  });

  it("ignores a reset increase when averaging", () => {
    const points = [
      { day: "2026-08-14", remainingPercent: 40 },
      { day: "2026-08-15", remainingPercent: 20 },
      { day: "2026-08-16", remainingPercent: 95 },
    ];
    const prediction = calculateQuotaPrediction(85, "2026-08-20T12:00:00Z", points, now);
    expect(prediction.averageDailyUsagePercent).toBe(15);
    expect(prediction.daysAtAverage).toBeCloseTo(5.6667, 4);
  });

  it("round-trips only validated local data", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
    };
    const history = { "codex:scope-a:604800": [{ day: "2026-08-17", remainingPercent: 42 }] };
    saveQuotaHistory(history, storage);
    expect(loadQuotaHistory(storage)).toEqual(history);
    values.delete("quota-pro:quota-history:v2");
    values.set("quota-pro:quota-history:v1", JSON.stringify({ "codex:scope-a:604800": [{ day: "2026-08-17", remainingPercent: 42 }] }));
    expect(loadQuotaHistory(storage)).toEqual({});
  });

  it("keeps history isolated between account scopes", () => {
    const accountA = quotaHistoryKey(snapshot, window)!;
    const accountB = quotaHistoryKey({ provider: "codex", quotaHistoryScope: "scope-b" }, window)!;
    expect(accountA).toBe("codex:scope-a:604800");
    expect(accountB).toBe("codex:scope-b:604800");

    let history: QuotaHistory = {};
    history = recordQuotaSample(history, accountA, 64, now);
    history = recordQuotaSample(history, accountB, 92, now);
    expect(history[accountA]).toEqual([{ day: "2026-08-17", remainingPercent: 64 }]);
    expect(history[accountB]).toEqual([{ day: "2026-08-17", remainingPercent: 92 }]);
  });

  it("assigns unscoped v1 history to the first identified account", () => {
    const values = new Map<string, string>([
      ["quota-pro:quota-history:v1", JSON.stringify({
        "codex:604800": [
          { day: "2026-08-15", remainingPercent: 65 },
          { day: "2026-08-16", remainingPercent: 50 },
          { day: "2026-08-17", remainingPercent: 40 },
        ],
        "codex:18000": [
          { day: "2026-08-17", remainingPercent: 80 },
        ],
      })],
    ]);
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
    };
    const scopedKey = quotaHistoryKey(snapshot, window)!;
    const current: QuotaHistory = {
      [scopedKey]: [{ day: "2026-08-17", remainingPercent: 42 }],
    };

    const migrated = assignLegacyQuotaHistoryToScope(current, snapshot, storage);

    expect(migrated[scopedKey]).toEqual([
      { day: "2026-08-15", remainingPercent: 65 },
      { day: "2026-08-16", remainingPercent: 50 },
      { day: "2026-08-17", remainingPercent: 42 },
    ]);
    expect(migrated["codex:scope-a:18000"]).toEqual([
      { day: "2026-08-17", remainingPercent: 80 },
    ]);
    expect(values.has("quota-pro:quota-history:v1")).toBe(false);
    expect(JSON.parse(values.get("quota-pro:quota-history:v2") ?? "{}"))
      .toEqual(migrated);

    const accountB = { provider: "codex" as const, quotaHistoryScope: "scope-b" };
    expect(assignLegacyQuotaHistoryToScope(migrated, accountB, storage))
      .toBe(migrated);
  });

  it("keeps unscoped v1 history untouched until an account is identified", () => {
    const values = new Map<string, string>([
      ["quota-pro:quota-history:v1", JSON.stringify({
        "codex:604800": [{ day: "2026-08-16", remainingPercent: 50 }],
      })],
    ]);
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
    };
    const history: QuotaHistory = {};

    expect(assignLegacyQuotaHistoryToScope(
      history,
      { provider: "codex", quotaHistoryScope: null },
      storage,
    )).toBe(history);
    expect(values.has("quota-pro:quota-history:v1")).toBe(true);
    expect(values.has("quota-pro:quota-history:v2")).toBe(false);
  });

  it("does not create a history key for an unidentified account", () => {
    expect(quotaHistoryKey({ provider: "codex", quotaHistoryScope: null }, window)).toBeNull();
  });
});
