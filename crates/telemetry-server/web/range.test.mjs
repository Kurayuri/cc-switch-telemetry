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
  selectCalendarRange,
  startOfLocalDayMs,
  timeInputPlaceholder,
  timeInputValue,
} from "./range.js";

test("custom range defaults to all of today in local time", () => {
  const now = new Date(2026, 6, 30, 15, 20).getTime();
  const range = defaultCustomRange(now);
  assert.equal(range.from, Math.floor(new Date(2026, 6, 30).getTime() / 1000));
  assert.equal(range.to, Math.floor(new Date(2026, 6, 30, 23, 59, 59).getTime() / 1000));
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
  assert.equal(timeInputPlaceholder("12h"), "hh:mm:ss AM/PM");
  assert.equal(timeInputPlaceholder("24h"), "HH:mm:ss");
  const midnight = Math.floor(new Date(2026, 6, 30, 0, 5).getTime() / 1000);
  const noon = Math.floor(new Date(2026, 6, 30, 12, 45).getTime() / 1000);
  assert.equal(timeInputValue(midnight, "12h"), "12:05:00 AM");
  assert.equal(timeInputValue(noon, "12h"), "12:45:00 PM");
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


test("custom time seconds round trip in both formats and reject invalid seconds", () => {
  const stamp = Math.floor(new Date(2026, 6, 30, 23, 59, 59).getTime() / 1000);
  for (const format of ["12h", "24h"]) {
    assert.equal(parseDateTimeParts("2026-07-30", timeInputValue(stamp, format), format), stamp);
  }
  assert.equal(timeInputValue(stamp), "23:59:59");
  assert.equal(timeInputValue(stamp, "12h"), "11:59:59 PM");
  assert.ok(Number.isNaN(parseDateTimeParts("2026-07-30", "23:59:60")));
  assert.ok(Number.isNaN(parseDateTimeParts("2026-07-30", "11:59:60 PM", "12h")));
});

test("calendar dates use complete days for repeated, reverse, and same-day ranges", () => {
  const day = (month, date) => new Date(2026, month - 1, date);
  const stamp = (month, date, h = 0, m = 0, s = 0) => new Date(2026, month - 1, date, h, m, s).getTime() / 1000;
  let range = defaultCustomRange(day(7, 30).getTime());
  range = selectCalendarRange(range, day(7, 5), "start");
  range = selectCalendarRange(range, day(7, 10), "end");
  assert.deepEqual(range, { from: stamp(7, 5), to: stamp(7, 10, 23, 59, 59) });
  range = selectCalendarRange(range, day(7, 20), "start");
  assert.deepEqual(range, { from: stamp(7, 20), to: stamp(7, 20, 23, 59, 59) });
  range = selectCalendarRange(range, day(7, 15), "end");
  assert.deepEqual(range, { from: stamp(7, 15), to: stamp(7, 20, 23, 59, 59) });
  range = selectCalendarRange(range, day(7, 31), "start");
  range = selectCalendarRange(range, day(8, 2), "end");
  assert.deepEqual(range, { from: stamp(7, 31), to: stamp(8, 2, 23, 59, 59) });
  range = selectCalendarRange(range, day(7, 31), "end");
  assert.deepEqual(range, { from: stamp(7, 31), to: stamp(7, 31, 23, 59, 59) });
});

test("whole-day defaults follow local calendar boundaries on DST dates", () => {
  for (const month of [2, 10]) {
    const day = month === 2 ? 8 : 1;
    const range = defaultCustomRange(new Date(2026, month, day, 12).getTime());
    assert.equal(range.from, new Date(2026, month, day).getTime() / 1000);
    assert.equal(range.to, new Date(2026, month, day + 1).getTime() / 1000 - 1);
    assert.equal(timeInputValue(range.to), "23:59:59");
  }
});

test("current-time membership includes boundaries and freshly resolved live presets", async () => {
  const { rangeIncludesNow, resolvePresetRange, resolveAllTimeRange, resolveResetRange } = await import("./range.js");
  const nowMs = Date.UTC(2026, 8, 26, 12, 30, 15, 900);
  const now = Math.floor(nowMs / 1000);
  for (const preset of ["today", "1h", "24h", "7d", "14d", "30d", "1y"]) {
    assert.equal(rangeIncludesNow(resolvePresetRange(preset, nowMs), nowMs), true, preset);
  }
  assert.equal(rangeIncludesNow(resolveAllTimeRange(now - 1000, nowMs), nowMs), true);
  assert.equal(rangeIncludesNow(resolveResetRange(now + 100, 1000, nowMs), nowMs), true);
  for (const [range, expected] of [
    [{ from: now - 60, to: now }, true],
    [{ from: now, to: now + 60 }, true],
    [{ from: now - 60, to: now + 60 }, true],
    [{ from: now - 60, to: now - 1 }, false],
    [{ from: now + 1, to: now + 60 }, false],
    [null, false],
  ]) assert.equal(rangeIncludesNow(range, nowMs), expected);
});
