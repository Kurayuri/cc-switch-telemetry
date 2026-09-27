import test from "node:test";
import assert from "node:assert/strict";
import { resetCycleGroups, resolveResetCycleChoice } from "./reset-cycle.js";

const now = 1_800_000_000, period = 18000, reset = now + 9000;
const record = (end, used = true) => ({ resetsAt: end, firstSampledAt: end - period + 10,
  lastSampledAt: end - period + 140, sampleCount: 3, firstUsageAt: used ? end - period + 10 : null });
const history = records => ({ providers: [{ nodeId: "a", providerId: "p", tiers: [
  { key: "5h", kind: "utilizationPercent", unit: "%", resets: records },
] }] });
const groups = (records, at = now) => resetCycleGroups(history(records), [], at * 1000);
const choose = (g, selection = {}, at = now) => resolveResetCycleChoice(g, selection, at * 1000);

test("entry defaults current; previous includes zero usage and shares exact identity", () => {
  const g = groups([record(reset - period, false), record(reset)]);
  const current = choose(g);
  assert.deepEqual(current.range, { from: reset - period, to: now });
  const previous = choose(g, { ...current.selection, mode: "last" });
  assert.deepEqual(previous.range, { from: reset - 2 * period, to: reset - period });
  assert.equal(previous.cycle.firstUsageAt, null);
  assert.equal(choose(g, { ...current.selection, providerKey: 'missing' }).range, null);
  assert.equal(choose(g, { ...current.selection, tierId: 'missing' }).range, null);
});

test("missing adjacent cycle is unavailable, not an older recorded cycle", () => {
  assert.equal(choose(groups([record(reset - 3 * period), record(reset)]), { mode: "last" }).range, null);
  assert.equal(choose(groups([record(reset - period)]), { mode: "last" }).range, null);
  assert.equal(choose([], { mode: "current" }).range, null);
});

test("shortcuts follow rollover, manual cycle stays fixed", () => {
  const before = groups([record(reset - period), record(reset)]);
  const selection = choose(before).selection;
  const at = reset + 500;
  const after = groups([record(reset - period), record(reset), record(reset + period)], at);
  assert.deepEqual(choose(after, selection, at).range, { from: reset, to: at });
  assert.deepEqual(choose(after, { ...selection, mode: "last" }, at).range, { from: reset - period, to: reset });
  assert.deepEqual(choose(after, { ...selection, mode: "manual" }, at).range, { from: reset - period, to: reset });
});

test("early reset including a zero-use new cycle closes manual and previous ranges", () => {
  const prior = choose(groups([record(reset)]));
  const early = now - 200;
  const g = groups([record(reset), record(early + period, false)]);
  assert.deepEqual(choose(g).range, { from: early, to: now });
  assert.deepEqual(choose(g, { mode: "last" }).range, { from: reset - period, to: early });
  assert.deepEqual(choose(g, { ...prior.selection, mode: "manual" }).range, { from: reset - period, to: early });
});

test("current-only live samples work without inventing a previous cycle", () => {
  const live = [{ nodeId: "a", providerId: "p", current: [
    { key: "5h", kind: "utilizationPercent", unit: "%", resetsAt: reset, utilizationPercent: 0 },
  ], series: [] }];
  const g = resetCycleGroups(null, live, now * 1000);
  assert.deepEqual(choose(g).range, { from: reset - period, to: now });
  assert.equal(choose(g, { mode: "last" }).range, null);
  live[0].current[0].resetsAt += 30;
  const stable = resetCycleGroups(history([record(reset)]), live, now * 1000);
  assert.equal(choose(stable).cycle.resetsAt, reset, "confirmed history wins over jitter");
});
