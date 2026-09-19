import assert from "node:assert/strict";
import test from "node:test";
import {
  filterQuotaProviders,
  quotaAmount,
  quotaAmountRange,
  quotaExhaustion,
  quotaMetricIdentity,
  quotaMetricValue,
  quotaPercentage,
  quotaPrediction,
  quotaPredictionRows,
  quotaResetTiers,
  quotaTierPeriodLabel,
  quotaTierPeriodSeconds,
  splitQuotaPoints,
} from "./quota-view.js";

const predictionPoints = (values) => values.map(([sampledAt, utilizationPercent, extra = {}]) => ({
  sampledAt, utilizationPercent, ...extra,
}));

test("exhaustion time stays separate from the reset-clipped prediction line", () => {
  for (const reset of [300, 540, 900]) {
    const points = predictionPoints([[0, 10], [60, 20]]).map((p) => ({ ...p, resetsAt: reset }));
    const prediction = quotaPrediction(points, 60);
    assert.equal(prediction.exhaustedAt, 540);
    assert.equal(prediction.end.at, Math.min(reset, 540));
    assert.deepEqual(quotaExhaustion(points.at(-1), prediction), {
      status: "estimated", at: 540, beforeReset: 540 < reset, afterReset: 540 > reset,
    });
  }
});

test("card exhaustion handles loading, failed, exhausted and invalid-reset metrics", () => {
  const metric = { sampledAt: 60, utilizationPercent: 20, resetsAt: 900 };
  assert.equal(quotaExhaustion(metric, null, { loading: true }).status, "loading");
  assert.equal(quotaExhaustion(metric, null, { loading: true, error: new Error("failed") }).status, "unavailable");
  assert.equal(quotaExhaustion(metric, null).status, "unavailable");
  assert.deepEqual(quotaExhaustion({ ...metric, utilizationPercent: 100 }, null), { status: "exhausted", beforeReset: true });
  assert.equal(quotaExhaustion({ sampledAt: 60, remaining: 0, resetsAt: 900 }, null).status, "exhausted");
  for (const resetsAt of [null, "", 0, NaN, Infinity, 60, 59, 9e15]) {
    assert.equal(quotaExhaustion({ ...metric, resetsAt }, null), null);
  }
});

test("exhaustion estimates reject stale prediction anchors and unknown depletion limits", () => {
  const metric = { sampledAt: 60, utilizationPercent: 20, resetsAt: 900 };
  const prediction = quotaPrediction(predictionPoints([[0, 10], [60, 20]]), 60);
  assert.equal(quotaExhaustion({ ...metric, sampledAt: 120 }, prediction).status, "unavailable");
  const used = [{ sampledAt: 0, used: 2, resetsAt: 900 }, { sampledAt: 60, used: 4, resetsAt: 900 }];
  const unlimited = quotaPrediction(used, 60);
  assert.equal(unlimited.exhaustedAt, null);
  assert.equal(quotaExhaustion(used.at(-1), unlimited).status, "unavailable");
});

test("exhaustion estimates retain reset jitter tolerance", () => {
  const points = predictionPoints([[0, 10, { resetsAt: 900 }], [60, 20, { resetsAt: 899 }]]);
  const prediction = quotaPrediction(points, 60);
  assert.equal(prediction.exhaustedAt, 540);
  assert.equal(quotaExhaustion(points.at(-1), prediction).beforeReset, true);
});

test("prediction uses the last two platform starts and anchors at the latest sample", () => {
  const points = predictionPoints([[0, 18], [60, 20], [120, 20], [180, 22], [240, 22]]);
  const original = structuredClone(points);
  const result = quotaPrediction(points, 60);
  assert.equal(result.slope, 2 / 120);
  assert.deepEqual(result.start, { at: 240, value: 22 });
  assert.deepEqual(result.end, { at: 4920, value: 100 });
  assert.deepEqual(points, original);
});

test("prediction stops at the earlier of exhaustion or reset", () => {
  const points = predictionPoints([[0, 20], [120, 22], [240, 22]])
    .map((point) => ({ ...point, resetsAt: 360 }));
  assert.deepEqual(quotaPrediction(points, 60).end, { at: 360, value: 24 });
  assert.equal(quotaPrediction(points.map((p) => ({ ...p, resetsAt: 9000 })), 60).end.value, 100);
  assert.equal(quotaPrediction(points.map((p) => ({ ...p, resetsAt: 240 })), 60), null);
});

test("prediction never crosses reset, refill, gap, segment or scale boundaries", () => {
  for (const points of [
    predictionPoints([[0, 20], [60, 22], [120, 0]]),
    predictionPoints([[0, 20, { resetsAt: 180 }], [60, 22, { resetsAt: 180 }], [120, 23, { resetsAt: 360 }]]),
    predictionPoints([[0, 20], [60, 22], [900, 23]]),
    predictionPoints([[0, 20, { segmentId: 1 }], [60, 22, { segmentId: 1 }], [120, 23, { segmentId: 2 }]]),
    [{ sampledAt: 0, remaining: 20 }, { sampledAt: 60, remaining: 18 }, { sampledAt: 120, remaining: 30 }],
    [{ sampledAt: 0, remaining: 20 }, { sampledAt: 60, remaining: 18 }, { sampledAt: 120, utilizationPercent: 30 }],
  ]) assert.equal(quotaPrediction(points, 60), null);
  const afterReset = quotaPrediction(predictionPoints([[0, 20], [60, 22], [120, 0], [180, 2]]), 60);
  assert.equal(afterReset.slope, 2 / 60);
  assert.deepEqual(afterReset.start, { at: 180, value: 2 });
});

test("prediction respects backend continuity for coarsely bucketed points", () => {
  assert.ok(quotaPrediction(predictionPoints([[0, 20, { segmentId: 1 }], [900, 22, { segmentId: 1 }]]), 900));
});

test("prediction does not revive an old scale after a metric switches axes", () => {
  const points = [...predictionPoints([[0, 20], [60, 22]]), { sampledAt: 120, remaining: 30 }];
  assert.equal(quotaPrediction(points, 60, quotaPercentage), null);
  assert.equal(quotaPrediction(points, 60, quotaAmount), null);
});

test("prediction needs finite changing data and a computable future endpoint", () => {
  for (const points of [[], predictionPoints([[0, 20]]), predictionPoints([[0, 20], [60, 20]]),
    predictionPoints([[0, 20], [0, 22]]), predictionPoints([[0, 20], [60, 100]]),
    [{ sampledAt: 0, used: 2 }, { sampledAt: 60, used: 4 }],
    predictionPoints([[0, null], [60, NaN]]),
  ]) assert.equal(quotaPrediction(points, 60), null);
  assert.deepEqual(quotaPrediction([
    { sampledAt: 0, used: 2, resetsAt: 120 }, { sampledAt: 60, used: 4, resetsAt: 120 },
  ], 60).end, { at: 120, value: 6 });
});

test("amount prediction reaches zero and derived usage retains percent semantics", () => {
  assert.deepEqual(quotaPrediction([
    { sampledAt: 0, remaining: 10 }, { sampledAt: 60, remaining: 8 }, { sampledAt: 120, remaining: 8 },
  ], 60).end, { at: 360, value: 0 });
  assert.deepEqual(quotaPrediction([
    { sampledAt: 0, remaining: 10, total: 20 }, { sampledAt: 60, remaining: 8, total: 20 },
  ], 60).end, { at: 300, value: 100 });
});

test("prediction tooltip rows honor visibility and never extrapolate past endpoints", () => {
  const prediction = quotaPrediction(predictionPoints([[0, 20], [120, 22]]), 60);
  const plots = [{ name: "node-a", prediction }, { name: "node-b", prediction }, { name: "disabled" }];
  assert.deepEqual(quotaPredictionRows(plots, 240, { "node-b": false }), [{ plot: plots[0], value: 24 }]);
  assert.deepEqual(quotaPredictionRows(plots, 60), []);
  assert.deepEqual(quotaPredictionRows(plots, prediction.end.at + 1), []);
});

test("node and provider filters never merge identities", () => {
  const providers = [
    { nodeId: "node-a", providerId: "provider-a" },
    { nodeId: "node-a", providerId: "provider-b" },
    { nodeId: "node-b", providerId: "provider-a" },
  ];
  assert.deepEqual(filterQuotaProviders(providers, "node-a", "provider-a"), [providers[0]]);
  assert.deepEqual(filterQuotaProviders(providers, "", "provider-a"), [providers[0], providers[2]]);
});

test("explicit and derivable utilization use the bounded percentage axis", () => {
  const percent = {
    key: "five-hour",
    kind: "utilizationPercent",
    unit: "%",
    utilizationPercent: 42.5,
  };
  const usedBalance = {
    key: "credits",
    kind: "balance",
    unit: "USD",
    used: 2,
    total: 20,
  };
  const remainingBalance = {
    key: "credits",
    kind: "balance",
    unit: "USD",
    remaining: 18,
    total: 20,
  };
  assert.equal(quotaPercentage(percent), 42.5);
  assert.equal(quotaPercentage(usedBalance), 10);
  assert.equal(quotaPercentage(remainingBalance), 10);
  assert.equal(quotaPercentage({ used: 30, total: 20 }), 100);
  assert.equal(quotaPercentage({ utilizationPercent: -5 }), 0);
  assert.equal(quotaAmount(remainingBalance), null);
  assert.notEqual(quotaMetricIdentity(percent), quotaMetricIdentity(remainingBalance));
});

test("only values without a usable denominator use the amount axis", () => {
  const balance = { remaining: 18, unit: "USD" };
  assert.equal(quotaPercentage(balance), null);
  assert.equal(quotaAmount(balance), 18);
  assert.equal(quotaMetricValue(balance), 18);
  assert.equal(quotaAmount({ remaining: 18, total: 0 }), 18);
  assert.deepEqual(quotaAmountRange([0, 0]), { minimum: 0, maximum: 1, span: 1 });
  assert.deepEqual(quotaAmountRange([-5, 10]), { minimum: -5, maximum: 10, span: 15 });
});

test("reset tiers recognize compact and named subscription windows", () => {
  assert.equal(quotaTierPeriodSeconds({ key: "subscription:five-hour", label: "five_hour" }), 5 * 60 * 60);
  assert.equal(quotaTierPeriodSeconds({ key: "subscription:seven-day", label: "seven_day" }), 7 * 24 * 60 * 60);
  assert.equal(quotaTierPeriodSeconds({ key: "subscription:custom", label: "2 weeks" }), 14 * 24 * 60 * 60);
  assert.equal(quotaTierPeriodLabel(5 * 60 * 60), "5h");
  assert.equal(quotaTierPeriodLabel(7 * 24 * 60 * 60), "7d");
});

test("reset tier candidates keep provider metrics separate and omit stale balances", () => {
  const provider = {
    nodeId: "node-a",
    providerId: "provider-a",
    current: [
      { key: "subscription:five-hour", kind: "utilizationPercent", unit: "%", sampledAt: 100, resetsAt: 18_100 },
      { key: "subscription:extra-usage", kind: "balance", unit: "USD", sampledAt: 100, resetsAt: null },
      { key: "subscription:seven-day", kind: "utilizationPercent", unit: "%", sampledAt: 200, resetsAt: 150 },
    ],
  };
  const tiers = quotaResetTiers(provider);
  assert.deepEqual(tiers.map((tier) => [tier.id, tier.periodLabel]), [
    [JSON.stringify(["subscription:five-hour", "utilizationPercent", "%"]), "5h"],
  ]);
});

test("reset tier periods can fall back to historical reset boundaries", () => {
  assert.equal(
    quotaTierPeriodSeconds(
      { key: "subscription:window", label: "Window" },
      [{ resetsAt: 100 }, { resetsAt: 700 }, { resetsAt: 1_300 }],
    ),
    600,
  );
});

test("reset jumps and short missing-sample intervals stay connected", () => {
  const points = [
    { sampledAt: 60, utilizationPercent: 91, resetsAt: 119 },
    { sampledAt: 120, utilizationPercent: 3, resetsAt: 600 },
    { sampledAt: 240, utilizationPercent: 8, resetsAt: 600 },
  ];
  const segments = splitQuotaPoints(points, 60);
  assert.deepEqual(segments.map((segment) => segment.map(quotaMetricValue)), [[91, 3, 8]]);
  assert.equal(segments[0][1].resetsAt, 600);
});

test("axis transitions break lines instead of connecting incompatible scales", () => {
  const points = [
    { sampledAt: 60, remaining: 18 },
    { sampledAt: 120, remaining: 18, total: 20 },
    { sampledAt: 180, remaining: 17 },
  ];
  assert.deepEqual(
    splitQuotaPoints(points, 60, quotaAmount).map((segment) => segment.map(quotaAmount)),
    [[18], [17]],
  );
  assert.deepEqual(
    splitQuotaPoints(points, 60, quotaPercentage).map((segment) => segment.map(quotaPercentage)),
    [[10]],
  );
});

test("quota continuity uses the inclusive ten-minute boundary", () => {
  const points = [100, 699, 1299, 1900].map((sampledAt) => ({ sampledAt, remaining: 10 }));
  assert.deepEqual(splitQuotaPoints(points, 60).map((segment) => segment.map((p) => p.sampledAt)), [[100, 699, 1299], [1900]]);
  assert.equal(splitQuotaPoints([{ sampledAt: 0, remaining: 1 }, { sampledAt: 200 }, { sampledAt: 600, remaining: 2 }], 60).length, 1);
});
test("server segments survive coarse downsampling and keep real gaps", () => {
  const points = [
    { sampledAt: 100, remaining: 1, segmentId: 1 },
    { sampledAt: 3600, remaining: 2, segmentId: 1 },
    { sampledAt: 4300, remaining: 3, segmentId: 2 },
  ];
  assert.deepEqual(splitQuotaPoints(points, 3600).map((segment) => segment.length), [2, 1]);
});
