import test from "node:test";
import assert from "node:assert/strict";
import {
  MAX_RANGE_DAYS,
  defaultCustomRange,
  parseBucketValue,
  resolvePresetRange,
  startOfLocalDayMs,
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
