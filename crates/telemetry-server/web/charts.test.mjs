import assert from "node:assert/strict";
import test from "node:test";
import {
  buildDailyOption,
  buildQuotaOption,
  buildTrendOption,
  accumulateTrendPoints,
  dailyCalendarLayout,
  escapeHtml,
  joinQuotaSegments,
  tooltipMarkup,
} from "./charts.js";

const palette = {
  text: "#fff",
  muted: "#999",
  faint: "#666",
  border: "rgba(255,255,255,.1)",
  borderStrong: "rgba(255,255,255,.2)",
  surfaceSolid: "#111",
  surfaceRaised: "#222",
  accent: "#0cc",
  accentArea: "rgba(0,204,204,.2)",
  accentShadow: "rgba(0,204,204,.3)",
  transparent: "rgba(0,0,0,0)",
};

test("tooltip markup escapes telemetry labels and values", () => {
  assert.equal(escapeHtml('<node id="a">&'), "&lt;node id=&quot;a&quot;&gt;&amp;");
  assert.equal(
    tooltipMarkup("<title>", [["provider", "A&B"]]),
    "<strong>&lt;title&gt;</strong><span><em>provider</em><b>A&amp;B</b></span>",
  );
});

test("trend options use a bounded time axis and retain source data for tooltips", () => {
  const point = { bucketStart: 10, realTotalTokens: 25 };
  const option = buildTrendOption({
    points: [point],
    metric: "realTotalTokens",
    range: { from: 5, to: 20 },
    palette,
    formatAxis: (value) => `at:${value}`,
    formatValue: (value) => `v:${value}`,
    formatTooltip: (source) => `tip:${source.realTotalTokens}`,
    ariaDescription: "Usage trend",
  });
  assert.equal(option.xAxis.min, 5_000);
  assert.equal(option.xAxis.max, 20_000);
  assert.deepEqual(option.series[0].data[0].value, [10_000, 25]);
  assert.equal(option.series[0].data[0].source, point);
  assert.equal(option.tooltip.formatter([{ data: option.series[0].data[0] }]), "tip:25");
  assert.equal(option.series[0].universalTransition, true);
});

test("cumulative trend points preserve buckets and aggregate totals", () => {
  const cumulative = accumulateTrendPoints([
    {
      bucketStart: 10,
      totalRequests: 2,
      successfulRequests: 1,
      freshInputTokens: 10,
      cacheCreationTokens: 2,
      cacheReadTokens: 8,
      outputTokens: 5,
      totalCostUsd: 0.25,
      avgLatencyMs: 100,
    },
    {
      bucketStart: 20,
      totalRequests: 0,
      successfulRequests: 0,
      freshInputTokens: 0,
      cacheCreationTokens: 0,
      cacheReadTokens: 0,
      outputTokens: 0,
      totalCostUsd: 0,
      avgLatencyMs: 999,
    },
    {
      bucketStart: 30,
      totalRequests: 3,
      successfulRequests: 3,
      freshInputTokens: 20,
      cacheCreationTokens: 3,
      cacheReadTokens: 7,
      outputTokens: 10,
      totalCostUsd: 0.75,
      avgLatencyMs: 200,
    },
  ]);
  assert.deepEqual(cumulative.map((point) => point.bucketStart), [10, 20, 30]);
  assert.equal(cumulative[0].realTotalTokens, 25);
  assert.equal(cumulative[1].realTotalTokens, 25);
  assert.equal(cumulative[2].realTotalTokens, 65);
  assert.equal(cumulative[2].totalRequests, 5);
  assert.equal(cumulative[2].successfulRequests, 4);
  assert.equal(cumulative[2].successRate, 80);
  assert.equal(cumulative[2].totalCostUsd, 1);
  assert.equal(cumulative[2].avgLatencyMs, 160);
  assert.equal(cumulative[2].cacheHitRate, 15 / 50);
});

test("quota segment conversion inserts nulls so missing samples never get connected", () => {
  const first = { sampledAt: 60, utilizationPercent: 20 };
  const second = { sampledAt: 240, utilizationPercent: 30 };
  const data = joinQuotaSegments([[first], [second]], (point) => point.utilizationPercent);
  assert.deepEqual(data.map((item) => item.value), [
    [60_000, 20],
    [150_000, null],
    [240_000, 30],
  ]);
});

test("quota options preserve percentage and amount axes", () => {
  const percent = { sampledAt: 60, utilizationPercent: 42 };
  const amount = { sampledAt: 60, remaining: 9 };
  const option = buildQuotaOption({
    plots: [
      {
        id: "percent",
        name: "Five hour · %",
        axis: "percent",
        color: "#0cc",
        value: (point) => point.utilizationPercent,
        segments: [[percent]],
      },
      {
        id: "amount",
        name: "Credits · USD",
        axis: "amount",
        color: "#99f",
        value: (point) => point.remaining,
        segments: [[amount]],
      },
    ],
    range: { from: 0, to: 120 },
    amountRange: { minimum: 0, maximum: 10, span: 10 },
    palette,
    formatAxis: String,
    formatAmount: String,
    formatTooltip: () => "tooltip",
    percentAxisName: "Usage",
    amountAxisName: "Balance",
    ariaDescription: "Quota chart",
  });
  assert.equal(option.yAxis[0].max, 100);
  assert.equal(option.yAxis[1].show, true);
  assert.equal(option.series[0].data[0].axis, "percent");
  assert.equal(option.series[1].data[0].axis, "amount");
  assert.equal(option.series[0].showSymbol, false);
  assert.equal(option.series[1].showSymbol, false);
  assert.equal(option.tooltip.trigger, "axis");
  assert.equal(option.series[1].lineStyle.type, "dashed");
});

test("quota predictions are dotted, share their actual legend and retain axis identity", () => {
  const plots = ["percent", "amount"].map((axis) => ({
    id: axis, name: axis, axis, color: "#0cc", segments: [], value: (point) => point.value,
    prediction: { start: { at: 60, value: 20 }, end: { at: 120, value: 30 }, slope: 1 / 6 },
  }));
  const options = {
    plots, range: { from: 0, to: 120 }, amountRange: { minimum: 0, maximum: 30 }, palette,
    formatAxis: String, formatAmount: String, formatTooltip: () => "tooltip",
    percentAxisName: "Usage", amountAxisName: "Balance", ariaDescription: "Quota chart",
  };
  const option = buildQuotaOption(options);
  assert.equal(option.series.length, 4);
  assert.deepEqual(option.legend.data, ["percent", "amount"]);
  for (const [actual, prediction] of [[option.series[0], option.series[1]], [option.series[2], option.series[3]]]) {
    assert.equal(prediction.name, actual.name);
    assert.equal(prediction.id, `${actual.id}:predict`);
    assert.equal(prediction.yAxisIndex, actual.yAxisIndex);
    assert.equal(prediction.lineStyle.color, actual.lineStyle.color);
    assert.equal(prediction.lineStyle.type, "dotted");
    assert.equal(prediction.showSymbol, false);
    assert.deepEqual(prediction.data.map((p) => p.value), [[60_000, 20], [120_000, 30]]);
    assert.equal(prediction.data[0].prediction, true);
  }
  assert.equal(option.yAxis[0].max, 100);
  assert.equal(option.xAxis.max, 120_000);
  assert.equal(option.xAxis.axisPointer.snap, false);
  assert.equal(buildQuotaOption({ ...options, plots: plots.map((p) => ({ ...p, prediction: null })) }).series.length, 2);
});

test("daily options use a calendar heatmap and quantized color dimension", () => {
  const point = { bucketStart: 1, realTotalTokens: 10 };
  const option = buildDailyOption({
    points: [{ date: "2026-09-01", value: 10, level: 3, source: point }],
    range: ["2026-01-01", "2026-12-31"],
    palette,
    colors: ["#0", "#1", "#2", "#3", "#4"],
    dayNames: ["S", "M", "T", "W", "T", "F", "S"],
    monthNames: ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"],
    formatTooltip: () => "tooltip",
    ariaDescription: "Daily usage",
  });
  assert.deepEqual(option.calendar.range, ["2026-01-01", "2026-12-31"]);
  assert.deepEqual(option.calendar.cellSize, [20, 20]);
  assert.equal(option.calendar.right, undefined);
  assert.equal(option.calendar.bottom, undefined);
  assert.equal(option.calendar.itemStyle.borderColor, palette.surfaceSolid);
  assert.equal(option.series[0].type, "heatmap");
  assert.equal(option.series[0].itemStyle.borderColor, palette.surfaceSolid);
  assert.deepEqual(option.series[0].data[0].value, ["2026-09-01", 10, 3]);
  assert.equal(option.visualMap.dimension, 2);
  assert.equal(option.visualMap.pieces[3].color, "#3");
});

test("daily calendar keeps square separated tiles while fitting narrow charts", () => {
  const desktop = dailyCalendarLayout(["2026-01-01", "2026-12-31"], 1120);
  const narrow = dailyCalendarLayout(["2026-01-01", "2026-12-31"], 320);
  assert.equal(desktop.cellSize, 20);
  assert.equal(desktop.borderWidth, 2);
  assert.equal(desktop.showLabels, true);
  assert.ok(desktop.left >= 48);
  assert.ok(narrow.cellSize < desktop.cellSize);
  assert.equal(narrow.borderWidth, 1);
  assert.equal(narrow.showLabels, false);
  assert.ok(narrow.left < 42);
  assert.ok(narrow.left + narrow.weekCount * narrow.cellSize <= 320);
});
