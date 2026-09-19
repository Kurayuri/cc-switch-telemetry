import test from "node:test";
import assert from "node:assert/strict";
import {
  MAX_RANGE_DAYS,
  defaultCustomRange,
  parseBucketValue,
  parseDateTimeParts,
  resolveResetRange,
  resolveAllTimeRange,
  resolvePresetRange,
  startOfLocalDayMs,
  timeInputPlaceholder,
  timeInputValue,
} from "./range.js";

test("custom range defaults to the latest complete local day", () => {
  const now = new Date(2026, 6, 30, 15, 20).getTime();
  const range = defaultCustomRange(now);
  assert.equal(range.from, Math.floor(new Date(2026, 6, 29).getTime() / 1000));
  assert.equal(range.to, Math.floor(startOfLocalDayMs(now) / 1000));
});

test("calendar presets start at local midnight", () => {
  const now = new Date(2026, 6, 30, 15, 20).getTime();
  const today = resolvePresetRange("today", now);
  const fortnight = resolvePresetRange("14d", now);
  assert.equal(today.from, Math.floor(startOfLocalDayMs(now) / 1000));
  assert.equal(fortnight.from, Math.floor(new Date(2026, 6, 17).getTime() / 1000));
  const year = resolvePresetRange("1y", now);
  assert.equal(year.from, Math.floor(new Date(2025, 6, 30).getTime() / 1000));
});

test("custom bucket values accept bounded integer units", () => {
  assert.equal(parseBucketValue("15", "m"), "15m");
  assert.equal(parseBucketValue("2", "h"), "2h");
  assert.equal(parseBucketValue("0", "s"), null);
  assert.equal(parseBucketValue("1.5", "h"), null);
  assert.equal(parseBucketValue(String(MAX_RANGE_DAYS), "d"), "720d");
  assert.equal(parseBucketValue(String(MAX_RANGE_DAYS + 1), "d"), null);
});

test("last reset range starts at the tier window and ends at now", () => {
  const nowMs = 1_800_000_000_000;
  const now = Math.floor(nowMs / 1000);
  const reset = now + 7 * 24 * 60 * 60 - 123;
  assert.deepEqual(resolveResetRange(reset, 7 * 24 * 60 * 60, nowMs), {
    from: reset - 7 * 24 * 60 * 60,
    to: now,
  });
  const shortReset = now + 5 * 60 * 60 - 123;
  assert.deepEqual(resolveResetRange(shortReset, 5 * 60 * 60, nowMs), {
    from: shortReset - 5 * 60 * 60,
    to: now,
  });
});

test("last reset range rejects stale, invalid, and oversized windows", () => {
  const nowMs = 1_800_000_000_000;
  const now = Math.floor(nowMs / 1000);
  assert.equal(resolveResetRange(now - 1, 60, nowMs), null);
  assert.equal(resolveResetRange(now + 60, 0, nowMs), null);
  assert.equal(resolveResetRange(now + 60, MAX_RANGE_DAYS * 24 * 60 * 60 + 1, nowMs), null);
});

test("custom time inputs support exact 12-hour midnight and noon values", () => {
  assert.equal(timeInputPlaceholder("12h"), "hh:mm AM/PM");
  assert.equal(timeInputPlaceholder("24h"), "HH:mm");
  const midnight = Math.floor(new Date(2026, 6, 30, 0, 5).getTime() / 1000);
  const noon = Math.floor(new Date(2026, 6, 30, 12, 45).getTime() / 1000);
  assert.equal(timeInputValue(midnight, "12h"), "12:05 AM");
  assert.equal(timeInputValue(noon, "12h"), "12:45 PM");
  assert.equal(
    parseDateTimeParts("2026-07-30", "12:05 AM", "12h"),
    midnight,
  );
  assert.equal(
    parseDateTimeParts("2026-07-30", "12:45 PM", "12h"),
    noon,
  );
});

test("custom time parsing rejects the wrong format and impossible values", () => {
  assert.equal(parseDateTimeParts("2026-07-30", "24:00", "12h"), Number.NaN);
  assert.equal(parseDateTimeParts("2026-07-30", "12:60 PM", "12h"), Number.NaN);
  assert.equal(parseDateTimeParts("2026-07-30", "12:05", "12h"), Number.NaN);
  assert.equal(parseDateTimeParts("2026-07-30", "23:59", "24h") > 0, true);
});

test("all time uses the earliest stored record including histories over 720 days", () => {
  const now = 1_800_000_000;
  assert.deepEqual(resolveAllTimeRange(now - 900 * 86400, now * 1000), { from: now - 900 * 86400, to: now });
  assert.deepEqual(resolveAllTimeRange(null, now * 1000), { from: now - 1, to: now });
  assert.deepEqual(resolveAllTimeRange(now, now * 1000), { from: now - 1, to: now });
});
