import assert from "node:assert/strict";
import test from "node:test";
import {
  filterQuotaProviders,
  quotaAmount,
  quotaAmountRange,
  quotaMetricIdentity,
  quotaMetricValue,
  quotaPercentage,
  splitQuotaPoints,
} from "./quota-view.js";

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
