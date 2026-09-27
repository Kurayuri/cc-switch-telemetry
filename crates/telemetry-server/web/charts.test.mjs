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

test("Usage axis fits the selected metric and cumulative values without rounding", () => {
  const points = [
    { bucketStart: 10, totalCostUsd: 0.25, totalRequests: 3 },
    { bucketStart: 20, totalCostUsd: 0.125, totalRequests: 2 },
  ];
  const args = { points, metric: "totalCostUsd", range: { from: 0, to: 30 },
    palette, formatAxis: String, formatValue: String, formatTooltip: String };
  assert.equal(buildTrendOption(args).yAxis.min, 0);
  assert.equal(buildTrendOption(args).yAxis.max, 0.25);
  assert.equal(buildTrendOption({ ...args, metric: "totalRequests" }).yAxis.max, 3);
  assert.equal(buildTrendOption({ ...args, points: accumulateTrendPoints(points) }).yAxis.max, 0.375);
  for (const values of [[], [0, 0], [NaN, Infinity, -Infinity], [Infinity, 0.125]]) {
    const option = buildTrendOption({ ...args,
      points: values.map((value, index) => ({ bucketStart: index, totalCostUsd: value })) });
    assert.equal(option.yAxis.max, values.includes(0.125) ? 0.125 : 1);
  }
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

test("comparison overlays unchanged quota history including drops and gaps", async () => {
  const { buildComparisonOption } = await import("./charts.js");
  const segments = [[{ sampledAt: 10, utilizationPercent: 80 }, { sampledAt: 20, utilizationPercent: 5 }],
    [{ sampledAt: 900, utilizationPercent: 15 }]];
  const plot = { id: "comparison-quota", name: "5h", color: "#a78bfa", axis: "percent",
    value: (point) => point.utilizationPercent, segments };
  const range = { from: 0, to: 1000 };
  const quota = buildQuotaOption({ plots: [plot], range, palette, formatAxis: String,
    formatAmount: String, percentAxisName: "%" });
  const points = accumulateTrendPoints([
    { bucketStart: 0, totalCostUsd: 2 }, { bucketStart: 500, totalCostUsd: 3 },
  ]);
  const option = buildComparisonOption({ points, metric: "totalCostUsd", range, palette,
    formatAxis: String, formatValue: String, formatTooltip: (p) => `cost:${p.totalCostUsd}`,
    quotaPlot: plot, usageLabel: "Cost", quotaLabel: "%",
    formatQuotaTooltip: (p) => `quota:${p.sampledAt}:${p.utilizationPercent}` });
  assert.deepEqual(option.series[1].data, quota.series[0].data);
  assert.deepEqual(option.series[0].data.map((p) => p.value), [[0, 2], [500000, 5]]);
  assert.deepEqual(option.series[1].data.map((p) => p.value[1]), [80, 5, null, 15]);
  assert.equal(option.yAxis[1].max, 80);
  assert.ok(Math.abs(option.yAxis[0].max - 5 * 80 / 15) < 1e-12);
  assert.ok(Math.abs(5 / option.yAxis[0].max - 15 / option.yAxis[1].max) < 1e-12);
  assert.equal(option.series[1].connectNulls, false);
  assert.equal(option.tooltip.formatter([
    { seriesId: "usage-trend", data: option.series[0].data[1] },
    { seriesId: "comparison-quota", data: option.series[1].data[1] },
  ]), "cost:5<br>quota:20:5");
});

test("comparison scales fractional/zero quota and reuses dotted predictions", async () => {
  const { buildComparisonOption } = await import("./charts.js");
  const plot = { id: "comparison-quota", name: "5h", color: "#a78bfa", axis: "percent",
    value: (point) => point.utilizationPercent,
    segments: [[{ sampledAt: 10, utilizationPercent: 0.2 }]],
  };
  const args = { points: [{ bucketStart: 10, totalCostUsd: 2 }], metric: "totalCostUsd",
    range: { from: 0, to: 200 }, palette, formatAxis: String, formatValue: String,
    formatTooltip: String, quotaPlot: plot, usageLabel: "Cost", quotaLabel: "%",
    formatQuotaTooltip: String, formatPredictionTooltip: (p) => `forecast:${p.value[1]}` };
  const fractionalAxis = buildComparisonOption(args).yAxis[1];
  assert.equal(fractionalAxis.max, 0.2);
  for (const [value, label] of [[0, "0%"], [0.2, "0%"], [33.49, "33%"], [33.5, "34%"], [125.75, "126%"]]) {
    assert.equal(fractionalAxis.axisLabel.formatter(value), label);
  }
  plot.segments[0][0].utilizationPercent = 0;
  assert.equal(buildComparisonOption(args).yAxis[1].max, 1);
  plot.prediction = { start: { at: 10, value: 0 }, end: { at: 190, value: 30 } };
  const option = buildComparisonOption(args);
  assert.equal(option.yAxis[1].max, 30);
  assert.equal(option.series[2].lineStyle.type, "dotted");
  assert.equal(option.series[2].yAxisIndex, 1);
  assert.deepEqual(option.series[2].data.map((p) => p.value), [[10000, 0], [190000, 30]]);
  assert.equal(option.tooltip.formatter([{ data: option.series[2].data[1] }]), "forecast:30");
});

test("comparison expands utilization beyond 100 to align the latest shared bucket", async () => {
  const { buildComparisonOption } = await import("./charts.js");
  const plot = { id: "comparison-quota", name: "7d", color: "#a78bfa", axis: "percent",
    value: (point) => point.utilizationPercent, segments: [[
      { sampledAt: 30, utilizationPercent: 10 },
      { sampledAt: 90, utilizationPercent: 20 },
      { sampledAt: 150, utilizationPercent: 40 },
    ]] };
  const usageRange = { from: 0, to: 180, bucket: "1m" };
  const option = buildComparisonOption({
    points: [{ bucketStart: 0, totalCostUsd: 10 }, { bucketStart: 60, totalCostUsd: 20 },
      { bucketStart: 120, totalCostUsd: 40 }],
    metric: "totalCostUsd", range: usageRange, usageRange, palette,
    formatAxis: String, formatValue: String, formatTooltip: String,
    quotaPlot: plot, usageLabel: "Cost", quotaLabel: "%", formatQuotaTooltip: String,
    estimatedQuotaEnabled: true,
    estimatedQuotaPlot: { name: "Estimated quota", color: "#fbbf24",
      points: [{ at: 60, value: 400 }] }, formatEstimatedQuotaTooltip: String,
  });
  assert.equal(option.yAxis[0].max, 400);
  assert.equal(option.yAxis[1].max, 400);
  assert.equal(40 / option.yAxis[0].max, 40 / option.yAxis[1].max);
});

test("enabled quota estimates retain a rendered 100% tick independently of estimate data", async () => {
  const { buildComparisonOption } = await import("./charts.js");
  const echarts = await import("./vendor/echarts.esm.min.mjs");
  const range = { from: 0, to: 60, bucket: "1m" };
  const args = {
    points: [{ bucketStart: 0, totalCostUsd: 40 }], metric: "totalCostUsd",
    range, usageRange: range, palette, formatAxis: String, formatValue: String,
    formatTooltip: String, usageLabel: "Cost", quotaLabel: "%", reducedMotion: true,
    estimatedQuotaEnabled: true, estimatedQuotaPlot: null,
    formatQuotaTooltip: String, formatEstimatedQuotaTooltip: String, formatPredictionTooltip: String,
  };
  const chart = echarts.init(null, null, { renderer: "svg", ssr: true, width: 800, height: 400 });
  try {
    for (const utilization of [0, 0.2, 60, 100, 100.1, 125, 137.4, 1e5]) {
      const quotaPlot = { id: "comparison-quota", name: "5h", color: palette.accent,
        value: (point) => point.utilizationPercent,
        segments: [[{ sampledAt: 30, utilizationPercent: utilization }]] };
      for (const prediction of [null, { start: { at: 30, value: utilization }, end: { at: 60, value: 150 } }]) {
        const option = buildComparisonOption({ ...args, quotaPlot: { ...quotaPlot, prediction } });
        const right = option.yAxis[1];
        assert.equal(right.max, Math.max(100, utilization, prediction?.end.value || 0));
        assert.ok(right.axisTick.customValues.includes(100));
        assert.ok(right.axisLabel.customValues.includes(100));
        assert.ok(right.axisLabel.customValues.every((value) => value === 100 || Math.abs(value - 100) >= right.max * 0.06));
        if (utilization > 0) assert.ok(Math.abs(40 / option.yAxis[0].max - utilization / right.max) < 1e-12);
        chart.setOption(option, { replaceMerge: ["series", "yAxis"] });
        const axis = chart.getModel().getComponent("yAxis", 1).axis;
        assert.ok(axis.getTicksCoords().some((tick) => tick.tickValue === 100));
        assert.ok(axis.getViewLabels().some((label) => label.formattedLabel === "100%"));
        assert.match(chart.renderToSVGString(), />100%<\/text>/);
      }
    }
    const quotaPlot = { id: "comparison-quota", name: "5h", value: (point) => point.utilizationPercent,
      segments: [[{ sampledAt: 30, utilizationPercent: 60 }]] };
    chart.setOption(buildComparisonOption({ ...args, quotaPlot }), { replaceMerge: ["series", "yAxis"] });
    const disabled = buildComparisonOption({ ...args, quotaPlot, estimatedQuotaEnabled: false });
    chart.setOption(disabled, { replaceMerge: ["series", "yAxis"] });
    const axis = chart.getModel().getComponent("yAxis", 1).axis;
    assert.equal(disabled.yAxis[1].max, 60);
    assert.ok(!chart.getOption().yAxis[1].axisLabel.customValues.includes(100));
    assert.deepEqual(chart.getOption().yAxis[1].axisLabel.customValues, chart.getOption().yAxis[1].axisTick.customValues);
    assert.ok(!axis.getViewLabels().some((label) => label.formattedLabel === "100%"));
    const unselected = buildComparisonOption(args);
    assert.equal(unselected.yAxis[1].max, 100);
    assert.ok(unselected.yAxis[1].axisLabel.customValues.includes(100));
  } finally {
    chart.dispose();
  }
});

test("comparison y axes and grid share pixel positions, then ordinary trend clears custom ticks", async () => {
  const { buildComparisonOption } = await import("./charts.js");
  const echarts = await import("./vendor/echarts.esm.min.mjs");
  const range = { from: 0, to: 60, bucket: "1m" };
  const base = { points: [{ bucketStart: 0, totalCostUsd: 40, totalRequests: 30, realTotalTokens: 3500 }],
    range, usageRange: range, palette, formatAxis: String, formatValue: String, formatTooltip: String,
    usageLabel: "Usage", quotaLabel: "%", reducedMotion: true,
    formatQuotaTooltip: String, formatEstimatedQuotaTooltip: String, formatPredictionTooltip: String };
  const chart = echarts.init(null, null, { renderer: "svg", ssr: true, width: 800, height: 400 });
  try {
    for (const metric of ["totalCostUsd", "totalRequests", "realTotalTokens"]) {
      for (const utilization of [null, 0, 60, 100, 137.4]) {
        for (const estimatedQuotaEnabled of [false, true]) {
          for (const predict of [false, true]) {
            const quotaPlot = utilization === null ? null : { id: "quota", name: "Quota",
              value: (point) => point.utilizationPercent,
              segments: [[{ sampledAt: 30, utilizationPercent: utilization }]],
              prediction: predict ? { start: { at: 30, value: utilization }, end: { at: 60, value: 175.3 } } : null };
            const option = buildComparisonOption({ ...base, metric, quotaPlot, estimatedQuotaEnabled,
              estimatedQuotaPlot: estimatedQuotaEnabled ? { name: "Estimate", color: palette.accent,
                points: [{ at: 30, value: 53.4 }] } : null });
            chart.setOption(option, { replaceMerge: ["series", "yAxis"] });
            const left = chart.getModel().getComponent("yAxis", 0);
            const grid = left.axis.getTicksCoords({ tickModel: left.getModel("splitLine") })
              .map((tick) => left.axis.toGlobalCoord(tick.coord));
            assert.equal(option.yAxis.filter((axis) => axis.splitLine.show).length, 1);
            for (let index = 0; index < option.yAxis.length; index++) {
              const axis = chart.getModel().getComponent("yAxis", index).axis;
              const ticks = axis.getTicksCoords().map((tick) => axis.toGlobalCoord(tick.coord));
              const labels = axis.getViewLabels().map((label) => axis.toGlobalCoord(axis.dataToCoord(label.tick.value)));
              assert.equal(ticks.length, grid.length);
              assert.equal(labels.length, grid.length);
              for (let i = 0; i < grid.length; i++) {
                assert.ok(Math.abs(ticks[i] - grid[i]) < 0.5, `${metric}: tick ${index}/${i}`);
                assert.ok(Math.abs(labels[i] - grid[i]) < 0.5, `${metric}: label ${index}/${i}`);
              }
            }
          }
        }
      }
    }
    const ordinary = buildTrendOption({ ...base, metric: "totalCostUsd" });
    chart.setOption(ordinary, { replaceMerge: ["series", "yAxis"] });
    const axes = chart.getOption().yAxis.filter(Boolean);
    assert.equal(axes.length, 1);
    assert.equal(axes[0].axisLabel.customValues, null);
    assert.equal(axes[0].axisTick.customValues, null);
    assert.equal(axes[0].splitLine.customValues, null);
    assert.equal(axes[0].axisTick.show, false);
    assert.equal(axes[0].axisLabel.hideOverlap, true);
  } finally {
    chart.dispose();
  }
});

test("comparison predictions expand Usage while actual shared samples remain the anchor", async () => {
  const { buildComparisonOption } = await import("./charts.js");
  const plot = { id: "comparison-quota", name: "7d", color: "#a78bfa", axis: "percent",
    value: (point) => point.utilizationPercent,
    segments: [[{ sampledAt: 30, utilizationPercent: 25 }, { sampledAt: 90, utilizationPercent: 50 }]],
    prediction: { start: { at: 90, value: 50 }, end: { at: 180, value: 100 } } };
  const usageRange = { from: 0, to: 120, bucket: "1m" };
  const option = buildComparisonOption({
    points: [{ bucketStart: 0, totalCostUsd: 10 }, { bucketStart: 60, totalCostUsd: 20 }],
    metric: "totalCostUsd", range: { ...usageRange, to: 190 }, usageRange, palette,
    formatAxis: String, formatValue: String, formatTooltip: String,
    quotaPlot: plot, usageLabel: "Cost", quotaLabel: "%", formatQuotaTooltip: String,
    formatPredictionTooltip: String,
  });
  assert.equal(option.yAxis[0].max, 40);
  assert.equal(option.yAxis[1].max, 100);
  assert.equal(20 / option.yAxis[0].max, 50 / option.yAxis[1].max);
});

test("comparison uses the latest shared bucket and does not bridge empty Usage buckets", async () => {
  const { buildComparisonOption } = await import("./charts.js");
  const plot = { id: "comparison-quota", name: "7d", color: "#a78bfa", axis: "percent",
    value: (point) => point.utilizationPercent,
    segments: [[{ sampledAt: 30, utilizationPercent: 10 }, { sampledAt: 90, utilizationPercent: 20 }]] };
  const args = { points: [{ bucketStart: 0, totalRequests: 1 }, { bucketStart: 60, totalRequests: 2 },
    { bucketStart: 120, totalRequests: 3 }], metric: "totalRequests",
    range: { from: 0, to: 180, bucket: "1m" }, palette,
    formatAxis: String, formatValue: String, formatTooltip: String,
    quotaPlot: plot, usageLabel: "Requests", quotaLabel: "%", formatQuotaTooltip: String };
  const option = buildComparisonOption(args);
  assert.equal(option.yAxis[0].max, 3);
  assert.equal(option.yAxis[1].max, 30);
  assert.equal(2 / option.yAxis[0].max, 20 / option.yAxis[1].max);

  const gap = buildComparisonOption({ ...args,
    points: [{ bucketStart: 0, totalRequests: 1 }, { bucketStart: 120, totalRequests: 3 }],
    quotaPlot: { ...plot, segments: [[{ sampledAt: 90, utilizationPercent: 20 }]] } });
  assert.equal(gap.yAxis[0].max, 3);
  assert.equal(gap.yAxis[1].max, 20);
});

test("Usage markers are visible through 120 points and hidden from 121", () => {
  for (const count of [119, 120, 121]) {
    const option = buildTrendOption({
      points: Array.from({ length: count }, (_, bucketStart) => ({ bucketStart, realTotalTokens: 1 })),
      metric: "realTotalTokens", range: { from: 0, to: count }, palette,
      formatAxis: String, formatValue: String, formatTooltip: String,
    });
    assert.equal(option.series[0].showSymbol, count <= 120);
    assert.equal(option.series[0].data.length, count);
  }
});

test("estimated quota uses the cost axis or a separate USD axis and preserves gaps", async () => {
  const { buildComparisonOption } = await import("./charts.js");
  for (const metric of ["totalCostUsd", "totalRequests", "realTotalTokens"]) {
    const args = { points: [{ bucketStart: 0, [metric]: 20 }], metric, range: { from: 0, to: 180 },
      palette, formatAxis: String, formatValue: String, formatTooltip: String,
      usageLabel: metric, quotaLabel: "%", formatMoney: (n) => `$${n}`,
      formatEstimatedQuotaTooltip: (p) => `estimate:${p.value}`, estimatedQuotaEnabled: true, estimatedQuotaPlot: {
        name: "Estimated quota", color: "#fbbf24", points: [
          { at: 60, value: 80 }, { at: 120, value: null }, { at: 180, value: 90 },
        ],
      } };
    const option = buildComparisonOption(args);
    const series = option.series.find((item) => item.id === "estimated-quota");
    assert.equal(option.yAxis.length, metric === "totalCostUsd" ? 2 : 3);
    assert.equal(series.yAxisIndex, metric === "totalCostUsd" ? 0 : 2);
    assert.equal(option.yAxis[series.yAxisIndex].max, 90);
    assert.deepEqual(series.data.map((p) => p.value), [[60000, 80], [120000, null], [180000, 90]]);
    assert.equal(series.connectNulls, false);
    assert.equal(series.lineStyle.type, "dashed");
    assert.equal(option.tooltip.formatter([{ data: series.data[0] }]), "estimate:80");
    assert.equal(option.tooltip.formatter([{ data: series.data[1] }]), "");
    const disabled = buildComparisonOption({ ...args, estimatedQuotaPlot: null, estimatedQuotaEnabled: false });
    assert.equal(disabled.yAxis.length, 2);
    assert.equal(disabled.series.length, 1);
    assert.equal(disabled.yAxis[0].max, 20);
  }
});
