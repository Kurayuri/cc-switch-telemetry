import test from "node:test";
import assert from "node:assert/strict";
import { confirmedResetObservations, pastResetGroups, resolvePastResetRange, choosePastReset, createPastResetLoader } from "./past-resets.js";

const now = 1_800_000_000;
const record = (resetsAt, firstSampledAt = resetsAt - 1000, lastSampledAt = firstSampledAt + 120, sampleCount = 3, firstUsageAt = firstSampledAt) => ({ resetsAt, firstSampledAt, lastSampledAt, sampleCount, firstUsageAt });
const provider = (tiers, nodeId = "node-a") => ({ nodeId, providerId: "shared", tiers });
const tier = (key, resets) => ({ key, kind: "utilizationPercent", unit: "%", resets });

test("confirmation requires three samples and ignores old-value returns", () => {
  const first = record(now, now - 5000);
  const next = record(now + 1000, now - 1000);
  const returned = record(now, now - 500);
  const noise = record(now + 2000, now - 300, now - 240, 2);
  const values = confirmedResetObservations([first, next, returned, noise]);
  assert.deepEqual(values.map((r) => r.resetsAt), [now, now + 1000]);
  assert.deepEqual(confirmedResetObservations([record(null), record(0)]), []);
  assert.equal(first.lastSampledAt, now - 4880, "input remains unchanged");
});

test("September early resets close prior cycles without requiring zero usage", () => {
  const stamp = (text) => Date.parse(`2026-${text}+08:00`) / 1000;
  const start8 = stamp("09-08T09:24:22");
  const start12 = stamp("09-12T16:10:07");
  const start19 = stamp("09-19T16:10:18");
  const week = 604800;
  const old = stamp("09-11T16:43:20");
  const values = [
    record(old, stamp("09-04T17:45:49")),
    record(start8 + week, start8 + 1),
    // Old readings persisted for 43 minutes; they still must not reopen that cycle.
    record(old, stamp("09-10T00:56:21"), stamp("09-10T01:39:21"), 44),
    record(start8 + week + 5, stamp("09-10T01:40:21")),
    record(start12 + week, start12 + 1),
    record(start19 + week, start19 + 1),
  ];
  const cycles = pastResetGroups({ providers: [provider([tier("7d", values)])] }, (start19 + 1000) * 1000)[0].tiers[0].cycles;
  assert.equal(cycles.length, 4);
  assert.deepEqual(cycles.map((c) => [c.from, c.endAt]), [
    [start19, start19 + week], [start12, start12 + week],
    [start8, start12], [old - week, start8],
  ]);
  assert.equal(resolvePastResetRange(cycles[2], (start19 + 1000) * 1000).to, start12);
});

test("zero-use windows close the previous cycle but are filtered only after detection", () => {
  const reset = now + 1000;
  const values = [record(now), record(reset, now - 100, now + 20, 3, null)];
  const groups = pastResetGroups({ providers: [provider([tier("1h", values)])] }, now * 1000);
  // The first sample's timestamp is outside the truncated first window, so use valid earlier evidence.
  values[0].firstUsageAt = now - 3000;
  const cycles = pastResetGroups({ providers: [provider([tier("1h", values)])] }, now * 1000)[0].tiers[0].cycles;
  assert.equal(groups.length, 0);
  assert.equal(cycles.length, 1);
  assert.equal(cycles[0].endAt, reset - 3600);
  const idle = Array.from({ length: 10 }, (_, index) => record(now + index * 120, now - 3600 + index * 120, now - 3540 + index * 120, 2, null));
  assert.deepEqual(confirmedResetObservations(idle), []);
  const frequentIdle = idle.map((run) => ({ ...run, sampleCount: 4 }));
  assert.deepEqual(confirmedResetObservations(frequentIdle), [], "frequent polling must outlast the jitter window");
});

test("historical and current 5h/7d cycles retain separate provider and metric identities", () => {
  const response = { providers: [provider([
    tier("subscription:five-hour", [record(now - 86400), record(now + 100), record(now + 101)]),
    tier("subscription:seven-day", [record(now - 604800), record(now + 500)]),
    tier("unknown", [record(now - 100)]),
  ]), provider([tier("subscription:five-hour", [record(now - 100)])], "node-b")] };
  const groups = pastResetGroups(response, now * 1000);
  assert.equal(groups.length, 2);
  assert.equal(groups[0].tiers.length, 2);
  const five = groups[0].tiers[0];
  assert.equal(five.periodSeconds, 18000);
  assert.equal(five.cycles.length, 2, "no fabricated cycles in observation gaps");
  assert.deepEqual(resolvePastResetRange(five.cycles[0], now * 1000), { from: now + 100 - 18000, to: now });
  assert.equal(groups[0].tiers[1].periodSeconds, 604800);
  const latest = choosePastReset(groups, null);
  assert.equal(latest.cycleId, String(now + 100));
  const old = choosePastReset(groups, { ...latest, cycleId: String(now - 86400) });
  assert.equal(old.cycle.resetsAt, now - 86400);
  assert.equal(choosePastReset(groups, { providerKey: groups[1].id }).providerKey, groups[1].id);
});

test("selected current cycle clamps at reset and never moves to another cycle", () => {
  const cycle = { from: now - 17900, resetsAt: now + 100 };
  assert.equal(resolvePastResetRange(cycle, now * 1000).to, now);
  assert.equal(resolvePastResetRange(cycle, (now + 100) * 1000).to, now + 100);
  assert.equal(resolvePastResetRange(cycle, (now + 86400) * 1000).to, now + 100);
  assert.equal(resolvePastResetRange({ from: now + 1, resetsAt: now + 10 }, now * 1000), null);
  assert.equal(resolvePastResetRange({ from: 0, resetsAt: now }, now * 1000), null);
});

test("unknown periods use merged history; missing or contradictory windows are unavailable", () => {
  const groups = pastResetGroups({ providers: [provider([
    tier("unknown", [record(now - 3600), record(now - 3599), record(now), record(now + 1)]),
    tier("empty", []), tier("single", [record(now)]),
    tier("5h", [record(now + 36000, now - 1)]),
  ])] }, now * 1000);
  assert.equal(groups[0].tiers.length, 1);
  assert.equal(groups[0].tiers[0].periodSeconds, 3600);
  assert.deepEqual(pastResetGroups({ providers: [] }), []);
  assert.equal(choosePastReset([], null), null);
});

test("load cancellation and stale responses cannot publish over a newer picker", async () => {
  const pending = [];
  const changes = [];
  const loader = createPastResetLoader((signal) => new Promise((resolve, reject) => pending.push({ signal, resolve, reject })), (value) => changes.push(value));
  const first = loader.load();
  const second = loader.load();
  assert.equal(pending[0].signal.aborted, true);
  pending[1].resolve({ providers: ["new"] });
  await second;
  pending[0].resolve({ providers: ["old"] });
  await first;
  assert.deepEqual(changes.at(-1).response, { providers: ["new"] });
  const third = loader.load();
  const before = changes.length;
  loader.cancel();
  pending[2].reject(new Error("late failure"));
  await third;
  assert.equal(changes.length, before);
});

test("failed requests can be retried", async () => {
  let attempts = 0;
  let latest;
  const loader = createPastResetLoader(async () => {
    if (++attempts === 1) throw new Error("unavailable");
    return { providers: [] };
  }, (value) => { latest = value; });
  await loader.load();
  assert.equal(latest.error, true);
  await loader.load();
  assert.deepEqual(latest, { loading: false, error: false, response: { providers: [] } });
});
