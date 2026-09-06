import assert from "node:assert/strict";
import test from "node:test";
import { providerIdentity, metricIdentity, providerName, providerSelected, metricSelected, mergeProviders } from "./quota-settings.js";
import { dailyTooltipPosition, usageTooltipMarkup } from "./charts.js";
const a = { nodeId: "a", providerId: "same", providerName: "Original" };
const b = { nodeId: "b", providerId: "same", providerName: "Original" };
const weekly = { key: "weekly", kind: "utilizationPercent", unit: "%" };
const balance = { key: "balance", kind: "balance", unit: "USD" };
test("aliases and per-provider metrics retain node identities", () => {
  const settings = { quotaProviderAliases: [{ ...a, alias: "Alpha" }, { ...b, alias: "Beta" }] };
  assert.notEqual(providerIdentity(a), providerIdentity(b));
  assert.equal(providerName(a, settings), "Alpha");
  assert.equal(providerName(b, settings), "Beta");
  const selection = [{ ...a, metrics: [weekly] }, { ...b, metrics: [balance] }];
  assert.equal(metricSelected(selection, a, weekly), true);
  assert.equal(metricSelected(selection, a, balance), false);
  assert.equal(metricSelected(selection, b, weekly), false);
  assert.equal(metricSelected(selection, b, balance), true);
  assert.equal(providerSelected([], a), false);
  assert.equal(providerSelected(null, a), true);
  assert.equal(metricSelected([{ ...a, metrics: [] }], a, weekly), false);
  assert.equal(metricSelected([{ ...a, metrics: null }], a, weekly), true);
  assert.equal(metricIdentity({ ...balance, unit: null }), metricIdentity({ ...balance, unit: "" }));
});
test("catalog refresh preserves configured unavailable providers and metrics", () => {
  const settings = { quotaDefaults: { providers: [{ ...a, metrics: [weekly] }, { ...b, metrics: [] }] } };
  const catalog = mergeProviders([{ ...a, current: [balance] }], settings);
  assert.equal(catalog.length, 2);
  assert.equal(catalog[1].unavailable, true);
  assert.equal(catalog[0].metrics.length, 2);
  assert.equal(catalog[0].metrics[1].unavailable, true);
  assert.equal(settings.quotaDefaults.providers[0].metrics.length, 1);
});
test("usage tooltips lead with exact total and preserve escaped date and detail", () => {
  const markup = usageTooltipMarkup({ realTotalTokens: 1234567 }, "<date>", [["Input", "123"]], (value) => new Intl.NumberFormat("en-US").format(value));
  assert.ok(markup.startsWith("<strong>Totel: 1,234,567</strong>"));
  assert.ok(markup.indexOf("&lt;date&gt;") < markup.indexOf("Input"));
  assert.ok(usageTooltipMarkup({}, "date", [], String).startsWith("<strong>Totel: 0</strong>"));
});
test("daily tooltip fits viewport even when calendar is near its bottom edge", () => {
  const origin = { left: 20, top: 650 };
  const dom = { ownerDocument: { documentElement: { clientWidth: 390, clientHeight: 844 }, getElementById: () => ({ getBoundingClientRect: () => origin }) } };
  const [x, y] = dailyTooltipPosition([310, 100], null, dom, null, { contentSize: [260, 300] });
  assert.ok(x + origin.left >= 12);
  assert.ok(y + origin.top >= 12);
  assert.ok(x + origin.left + 260 <= 378);
  assert.ok(y + origin.top + 300 <= 832);
});
