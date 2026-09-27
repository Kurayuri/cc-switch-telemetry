import test from "node:test";
import assert from "node:assert/strict";
import { createHistoryQuotaService, createHistoryQuotaLoader, historyCandidates, historyIdentity, historyRequestKey } from "./history-quota.js";
import { buildComparisonOption, buildTrendOption } from "./charts.js";
const provider = { nodeId: "node", providerId: "quota" };
const metric = { key: "5h", kind: "utilizationPercent", unit: "%" };
const record = (resetsAt) => ({ resetsAt, firstSampledAt: resetsAt - 18000, lastSampledAt: resetsAt - 17800, sampleCount: 3 });
const history = { providers: [{ ...provider, tiers: [{ ...metric, resets: [18000, 33000, 51000].map(record) }] }] };
const cycles = [{ from: 0, to: 15000, resetsAt: 18000 }, { from: 15000, to: 33000, resetsAt: 33000 }, { from: 33000, to: 51000, resetsAt: 51000 }];
const context = { provider, metric, range: { from: 33000, to: 40000 }, params: new URLSearchParams("node_id=usage&provider_id=billing&model=x&all_time=true"), billing: [] };
const signal = () => new AbortController().signal;
const turn = () => new Promise((resolve) => setImmediate(resolve));

test("previous cycles follow viewed exclusive end; manual cycle stays fixed; zero-use boundaries retained", () => {
  assert.deepEqual(historyCandidates(cycles, { mode: "previous" }, context.range, 40000), [cycles[1]]);
  assert.deepEqual(historyCandidates(cycles, { mode: "full" }, { from: 0, to: 33000 }, 40000), [cycles[0]]);
  assert.deepEqual(historyCandidates(cycles, { mode: "previous" }, { from: 33000, to: 90000 }, 40000), [cycles[1]]);
  assert.deepEqual(historyCandidates(cycles, { mode: "cycle", cycleId: "18000" }, context.range, 40000), [cycles[0]]);
  assert.deepEqual(historyCandidates(cycles, { mode: "previous" }, { from: 60000, to: 70000 }, 70000), []);
});

test("full source scans raw summaries and pairs cost through last valid sample second; caches and invalidates", async () => {
  const requests = [], bodies = [];
  const service = createHistoryQuotaService(async (url) => {
    requests.push(url);
    return url.includes("/resets?") ? history : { summary: { totalCostUsd: 40 } };
  }, async (url, body) => {
    bodies.push(body);
    return { cycles: body.cycles.map((c) => ({ ...c, reachedFull: c.from === 0, sampledAt: c.to - 100, utilizationPercent: 50 })) };
  }, () => 40000000);
  const result = await service.resolve(context, { mode: "full" }, signal());
  assert.equal(result.reference.amount, 80);
  assert.equal(result.reference.from, 0);
  assert.deepEqual(bodies[0].cycles, [cycles[1], cycles[0]]);
  const params = new URL(requests[1], "http://local").searchParams;
  assert.equal(params.get("from"), "0"); assert.equal(params.get("to"), "14901");
  assert.equal(params.get("node_id"), "usage"); assert.equal(params.get("provider_id"), "billing");
  assert.equal(params.get("model"), "x"); assert.equal(params.has("all_time"), false);
  await service.resolve(context, { mode: "full" }, signal());
  assert.equal(requests.length, 2); assert.equal(bodies.length, 1);
  await service.resolve({ ...context, billing: [2] }, { mode: "full" }, signal());
  assert.equal(requests.length, 3); assert.equal(bodies.length, 1);
  service.invalidate(); await service.resolve(context, { mode: "full" }, signal());
  assert.equal(bodies.length, 2);
  const wrong = await service.resolve(context, { mode: "cycle", cycleId: 18000, identity: "wrong" }, signal());
  assert.equal(wrong.reference, null); assert.equal(wrong.reason, "chooseCycle");
});

test("direct previous does not skip unavailable cycle; manual requires finite positive amount", async () => {
  let costs = 0;
  const service = createHistoryQuotaService(async (url) => {
    if (url.includes("/resets?")) return history;
    costs++; return { summary: { totalCostUsd: 0 } };
  }, async (url, body) => ({ cycles: body.cycles.map((c) => ({ ...c, utilizationPercent: null })) }), () => 40000000);
  assert.equal((await service.resolve(context, { mode: "previous" }, signal())).reference, null);
  assert.equal(costs, 0);
  for (const amount of [0, -1, "", "abc", Infinity]) assert.equal((await service.resolve(context, { mode: "manual", amount }, signal())).reference, null);
  assert.equal((await service.resolve(context, { mode: "manual", amount: "80" }, signal())).reference.amount, 80);
  const fixed = { mode: "cycle", cycleId: 18000, identity: historyIdentity(context) };
  assert.equal(historyRequestKey(context, fixed), historyRequestKey({ ...context, range: { from: 0, to: 100 } }, fixed));
  assert.notEqual(historyRequestKey(context, { mode: "full" }), historyRequestKey({ ...context, range: { from: 0, to: 100 } }, { mode: "full" }));
});

test("batches full-cycle search at 128 without one request per cycle", async () => {
  const resets = Array.from({ length: 260 }, (_, i) => record((i + 1) * 18000));
  const batches = [];
  const service = createHistoryQuotaService(async (url) => url.includes("/resets?")
    ? { providers: [{ ...provider, tiers: [{ ...metric, resets }] }] } : { summary: { totalCostUsd: 80 } },
  async (url, body) => { batches.push(body.cycles.length); return { cycles: body.cycles.map((c) => ({ ...c,
    reachedFull: c.from === 0, utilizationPercent: 100, sampledAt: c.to - 1 })) }; }, () => 4679999000);
  assert.equal((await service.resolve({ ...context, range: { from: 0, to: 4680000 } }, { mode: "full" }, signal())).reference.amount, 80);
  assert.deepEqual(batches, [128,128,3]);
});

test("late results cannot replace new selection, cancellation clears reference", async () => {
  const pending = []; let updates = 0;
  const loader = createHistoryQuotaLoader({ resolve: (ctx, cfg, signal) => new Promise((resolve) => pending.push({ resolve, signal })) }, () => updates++);
  loader.ensure(context, { mode: "manual", amount: 80 });
  loader.ensure(context, { mode: "manual", amount: 40 });
  assert.equal(pending[0].signal.aborted, true);
  pending[1].resolve({ reference: { amount: 40 } }); await turn();
  pending[0].resolve({ reference: { amount: 80 } }); await turn();
  assert.equal(loader.ensure(context, { mode: "manual", amount: 40 }).reference.amount, 40);
  assert.equal(updates, 1); loader.cancel();
  assert.equal(loader.ensure(context, { mode: "manual", amount: 40 }).reference, null);
});

const palette = { text: "#fff", muted: "#aaa", accent: "#0cc", border: "#333" };
test("reference fixes dollars to 100%, contains data and aligns ticks for every metric and estimate state", () => {
  for (const metric of ["totalCostUsd", "realTotalTokens", "totalRequests"]) for (const estimate of [false, true]) {
    const args = { metric, points: [{ bucketStart: 0, [metric]: 160 }], range: { from: 0, to: 100, bucketSeconds: 100 }, palette,
      chartWidth: 390, formatAxis: String, formatValue: String, formatTooltip: String, formatMoney: String,
      usageLabel: metric, quotaLabel: "%", historyReferenceEnabled: true, historyReference: { amount: 80, name: "History", color: "#0f0" },
      estimatedQuotaEnabled: estimate, estimatedQuotaPlot: estimate ? { name: "Estimate", color: "#ff0", points: [{ at: 50, value: 240 }] } : null,
      quotaPlot: { id: "quota", axis: "percent", name: "Quota", value: (p) => p.utilizationPercent, segments: [[{ sampledAt: 50, utilizationPercent: 60 }]], prediction: { start: { at: 50, value: 60 }, end: { at: 100, value: 150 } } } };
    const option = buildComparisonOption(args), usd = metric === "totalCostUsd" ? 0 : 2;
    assert.equal(option.yAxis[usd].max / option.yAxis[1].max, .8);
    assert.ok(option.yAxis[1].max >= 150);
    assert.ok(option.yAxis[1].axisLabel.customValues.includes(100));
    for (const axis of option.yAxis) assert.deepEqual(axis.axisLabel.customValues.map((x) => +(x / axis.max).toFixed(9)), option.yAxis[1].axisLabel.customValues.map((x) => +(x / option.yAxis[1].max).toFixed(9)));
    assert.equal(option.series.find((s) => s.id === "history-quota").yAxisIndex, usd);
    if (usd === 2) assert.equal(option.yAxis[0].max, buildTrendOption(args).yAxis.max);
    const pending = buildComparisonOption({ ...args, estimatedQuotaPlot: null, historyReference: null });
    assert.equal(pending.yAxis[0].max, buildTrendOption(args).yAxis.max, "pending history must not apply latest-bucket alignment");
    assert.ok(!pending.series.some((s) => s.id === "history-quota"));
  }
});
