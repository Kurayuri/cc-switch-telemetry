import assert from "node:assert/strict";
import test from "node:test";
import {
  buildDailyOption,
  buildQuotaOption,
  buildTrendOption,
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
