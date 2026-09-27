import assert from "node:assert/strict";
import test from "node:test";
import { createEstimatedQuotaLoader, estimatedQuotaCycles, estimatedQuotaPoints } from "./estimated-quota.js";

const provider = { nodeId: "node", providerId: "quota" };
const metric = { key: "5h", kind: "utilizationPercent", unit: "%" };
const record = (resetsAt, firstSampledAt = resetsAt - 18000) => ({
  resetsAt, firstSampledAt, lastSampledAt: firstSampledAt + 120, sampleCount: 3,
});
const history = (resets) => ({ providers: [{ ...provider, tiers: [{ ...metric, resets }] }] });
const range = { from: 60, to: 180, bucket: "1m" };
const sample = (sampledAt, utilizationPercent, extra = {}) => ({
  sampledAt, utilizationPercent, resetsAt: 18000, segmentId: 1, ...extra,
});
const overview = { range, summary: { totalCostUsd: 30 }, trend: [
  { bucketStart: 60, totalCostUsd: 10 }, { bucketStart: 120, totalCostUsd: 20 },
] };
const input = { cycle: { from: 0, to: 18000, resetsAt: 18000 }, overview,
  prefixCost: 10, quotaPoints: [sample(90, 25), sample(150, 50)], range };
const turn = () => new Promise((resolve) => setImmediate(resolve));

test("cycles require confirmed reset anchors and close at early resets", () => {
  const response = history([record(18000), record(33000), { ...record(51000), sampleCount: 2 }]);
  assert.deepEqual(estimatedQuotaCycles(response, provider, metric, { from: 100, to: 40000 }), [
    { from: 0, to: 15000, resetsAt: 18000 }, { from: 15000, to: 33000, resetsAt: 33000 },
  ]);
  assert.deepEqual(estimatedQuotaCycles(response, { ...provider, nodeId: "other" }, metric, range), []);
});

test("mid-cycle chart includes prefix cost and divides by a percentage fraction", () => {
  const points = estimatedQuotaPoints(input);
  assert.deepEqual(points.map((point) => [point.at, point.cost, point.value]), [[120, 20, 80], [180, 40, 80]]);
  assert.deepEqual(points.map((point) => point.sampledAt), [90, 150]);
  assert.equal(points[0].cycleFrom, 0);
});

test("zero/invalid utilization, missing samples and nonfinite cost produce gaps", () => {
  for (const quotaPoints of [[], [sample(90, 0)], [sample(90, null)], [sample(90, 25, { resetsAt: 36000 })]]) {
    assert.ok(estimatedQuotaPoints({ ...input, quotaPoints }).every((point) => point.value === null));
  }
  assert.ok(estimatedQuotaPoints({ ...input, prefixCost: Infinity }).every((point) => point.value === null));
  const points = estimatedQuotaPoints({ ...input, quotaPoints: [sample(90, 25)] });
  assert.equal(points[0].value, 80);
  assert.equal(points[1].value, null, "must not carry an old utilization into the next bucket");
});

test("segment changes break lines and sample matching uses half-open buckets", () => {
  const points = estimatedQuotaPoints({ ...input, quotaPoints: [sample(90, 25), sample(120, 50, { segmentId: 2 })] });
  assert.deepEqual(points.map((point) => point.value), [80, null, 80]);
  const clipped = estimatedQuotaPoints({ ...input,
    cycle: { ...input.cycle, from: 80, to: 160 },
    overview: { ...overview, range: { ...range, from: 80, to: 160 } } });
  assert.equal(clipped.at(-1).at, 160);
});

test("calendar-month buckets end on the actual next month", () => {
  const from = Date.UTC(2026, 1, 1) / 1000;
  const to = Date.UTC(2026, 2, 1) / 1000;
  const points = estimatedQuotaPoints({ cycle: { from, to, resetsAt: to }, range: { from, to },
    overview: { range: { from, to, bucket: "1mo" }, trend: [{ bucketStart: from, totalCostUsd: 20 }] },
    quotaPoints: [sample(to - 60, 25, { resetsAt: to })] });
  assert.equal(points[0].at, to);
  assert.equal(points[0].value, 80);
});

test("loader fills the prefix, reuses visible overview and preserves filters", async () => {
  const requests = [];
  let changes = 0;
  const loader = createEstimatedQuotaLoader(async (url) => {
    requests.push(url);
    return url.endsWith("resets") ? history([record(18000)]) : { summary: { totalCostUsd: 10 } };
  }, () => changes++);
  const args = { provider, metric: { ...metric, points: input.quotaPoints }, overview,
    params: new URLSearchParams("node_id=usage-node&provider_id=usage-provider&model=x&all_time=true") };
  assert.equal(loader.ensure("a", args).loading, true);
  await turn();
  assert.equal(changes, 1);
  assert.deepEqual(loader.ensure("a", args).points.map((p) => p.value), [80, 80]);
  assert.equal(requests.length, 2);
  const params = new URL(requests[1], "http://local").searchParams;
  assert.equal(params.get("from"), "0");
  assert.equal(params.get("to"), "60");
  assert.equal(params.get("bucket"), "auto");
  assert.equal(params.get("node_id"), "usage-node");
  assert.equal(params.get("provider_id"), "usage-provider");
  assert.equal(params.get("model"), "x");
  assert.equal(params.has("all_time"), false);
});

test("loader restarts cost for each cycle and inserts reset gaps", async () => {
  const fullRange = { from: 0, to: 36000, bucket: "1h" };
  const args = { provider, metric: { ...metric, points: [sample(17990, 50), sample(35990, 25, { resetsAt: 36000 })] },
    overview: { range: fullRange }, params: new URLSearchParams() };
  const loader = createEstimatedQuotaLoader(async (url) => {
    if (url.endsWith("resets")) return history([record(18000), record(36000)]);
    const query = new URL(url, "http://local").searchParams;
    const from = Number(query.get("from"));
    return { range: { ...fullRange, from, to: from + 18000 }, trend: [{ bucketStart: from + 14400, totalCostUsd: 20 }] };
  }, () => {});
  loader.ensure("a", args);
  await turn();
  assert.deepEqual(loader.ensure("a", args).points.map((p) => p.value), [40, null, 80]);
});

test("loader ignores stale completions and reports failure without automatic retry", async () => {
  const pending = [];
  let changes = 0;
  const loader = createEstimatedQuotaLoader((url, signal) => new Promise((resolve, reject) => pending.push({ resolve, reject, signal })), () => changes++);
  const args = { provider, metric, overview, params: new URLSearchParams() };
  loader.ensure("old", args);
  loader.ensure("new", args);
  assert.equal(pending[0].signal.aborted, true);
  pending[0].resolve(history([record(18000)]));
  pending[1].reject(new Error("offline"));
  await turn();
  assert.equal(changes, 1);
  assert.equal(loader.ensure("new", args).error, true);
  assert.equal(pending.length, 2);
  loader.cancel();
  loader.ensure("new", args);
  assert.equal(pending.length, 3);
  loader.cancel();
  pending[2].resolve(history([]));
  await turn();
  assert.equal(changes, 1);
});

test("cycle fetch concurrency never exceeds three", async () => {
  const cycles = Array.from({ length: 8 }, (_, index) => record((index + 1) * 18000));
  let active = 0;
  let maximum = 0;
  const loader = createEstimatedQuotaLoader(async (url) => {
    if (url.endsWith("resets")) return history(cycles);
    active++;
    maximum = Math.max(maximum, active);
    await turn();
    active--;
    return { range: { from: 0, to: 18000, bucket: "1h" }, trend: [] };
  }, () => {});
  const args = { provider, metric: { ...metric, points: [] },
    overview: { range: { from: 0, to: 144000, bucket: "1h" } }, params: new URLSearchParams() };
  loader.ensure("a", args);
  for (let i = 0; i < 10 && loader.ensure("a", args).loading; i++) await turn();
  assert.equal(maximum, 3);
  assert.equal(loader.ensure("a", args).loading, false);
});

test("refresh retains plotted points, reuses unchanged data, and revalidates on demand/TTL", async () => {
  let time = 1000, requests = 0, changes = 0;
  const pending = [];
  const loader = createEstimatedQuotaLoader(async (url) => {
    requests++;
    if (url.endsWith("resets")) return history([record(18000)]);
    return new Promise((resolve, reject) => pending.push({ resolve, reject }));
  }, () => changes++, () => time);
  const args = { provider, metric: { ...metric, points: input.quotaPoints }, overview, params: new URLSearchParams() };
  loader.ensure("selection", args);
  await turn(); pending[0].resolve({ summary: { totalCostUsd: 10 } }); await turn();
  const first = loader.ensure("selection", args).points;
  assert.equal(first[0].value, 80);
  loader.ensure("selection", { ...args, overview: structuredClone(overview), metric: structuredClone(args.metric) });
  assert.equal(requests, 2, "identical data cannot refetch or recompute");
  loader.revalidate();
  const loading = loader.ensure("selection", args);
  assert.equal(loading.loading, true);
  assert.equal(loading.points, first, "manual refresh must not remove the series");
  await turn(); pending[1].reject(new Error("offline")); await turn();
  assert.equal(loader.ensure("selection", args).points, first, "failure retains old plot");
  assert.equal(loader.ensure("selection", args).error, true);
  loader.revalidate(); loader.ensure("selection", args);
  await turn(); pending[2].resolve({ summary: { totalCostUsd: 20 } }); await turn();
  assert.equal(loader.ensure("selection", args).points[0].value, 120);
  time += 300001;
  assert.equal(loader.ensure("selection", args).loading, true);
  await turn(); pending[3].resolve({ summary: { totalCostUsd: 20 } }); await turn();
  assert.equal(changes, 4);
});

test("new tail data reuses unchanged cycle points and reset history", async () => {
  let resets = 0;
  const loader = createEstimatedQuotaLoader(async (url) => {
    if (url.endsWith("resets")) { resets++; return history([record(18000)]); }
    return { summary: { totalCostUsd: 10 } };
  }, () => {});
  const args = { provider, metric: { ...metric, points: input.quotaPoints }, overview, params: new URLSearchParams() };
  loader.ensure("selection", args); await turn();
  const changed = { ...args, overview: { ...overview, range: { ...range, to: 240 },
    trend: [...overview.trend, { bucketStart: 180, totalCostUsd: 20 }] },
    metric: { ...args.metric, points: [...args.metric.points, sample(210, 60)] } };
  assert.equal(loader.ensure("selection", changed).points.length, 2);
  await turn();
  assert.equal(loader.ensure("selection", changed).points.length, 3);
  assert.equal(resets, 1);
});
