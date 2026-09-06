const FONT_FAMILY = 'Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif';
const DAY_MS = 86_400_000;
const DAILY_LABEL_SPACE = 48;
const DAILY_COMPACT_WIDTH = 480;
const DAILY_MIN_CELL_SIZE = 3;
const DAILY_MAX_CELL_SIZE = 20;

function animationOptions(reducedMotion, pointCount) {
  const enabled = !reducedMotion && pointCount <= 2_000;
  return {
    animation: enabled,
    animationThreshold: 2_000,
    animationDuration: enabled ? 720 : 0,
    animationDurationUpdate: enabled ? 420 : 0,
    animationEasing: "cubicOut",
    animationEasingUpdate: "cubicInOut",
  };
}

function axisLine(palette) {
  return {
    axisLine: { show: false },
    axisTick: { show: false },
    axisLabel: {
      color: palette.muted,
      fontFamily: FONT_FAMILY,
      fontSize: 11,
      hideOverlap: true,
    },
    splitLine: {
      show: true,
      lineStyle: { color: palette.border, width: 1 },
    },
  };
}

function tooltipOptions(palette, formatter, trigger = "axis") {
  return {
    trigger,
    confine: true,
    className: "echarts-tooltip",
    backgroundColor: palette.surfaceSolid,
    borderColor: palette.borderStrong,
    borderWidth: 1,
    padding: [12, 14],
    textStyle: {
      color: palette.text,
      fontFamily: FONT_FAMILY,
      fontSize: 12,
      lineHeight: 19,
    },
    axisPointer: {
      type: "line",
      lineStyle: { color: palette.accent, opacity: 0.42, width: 1 },
    },
    formatter,
  };
}

function ariaOptions(description) {
  return {
    enabled: true,
    description,
    decal: { show: false },
  };
}

export function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

export function tooltipMarkup(title, lines) {
  const rows = (lines || [])
    .map(([label, value]) => label
      ? `<span><em>${escapeHtml(label)}</em><b>${escapeHtml(value)}</b></span>`
      : `<span>${escapeHtml(value)}</span>`)
    .join("");
  return `<strong>${escapeHtml(title)}</strong>${rows}`;
}

export function buildTrendOption({
  points,
  metric,
  range,
  chartWidth = 840,
  palette,
  formatAxis,
  formatValue,
  formatTooltip,
  ariaDescription,
  reducedMotion = false,
}) {
  const data = (points || []).map((point) => ({
    id: String(point.bucketStart),
    name: String(point.bucketStart),
    value: [Number(point.bucketStart) * 1_000, Number(point[metric] || 0)],
    source: point,
  }));
  const rangeFrom = Number(range.from) * 1_000;
  const rangeTo = Math.max(Number(range.to) * 1_000, rangeFrom + 1_000);
  return {
    ...animationOptions(reducedMotion, data.length),
    aria: ariaOptions(ariaDescription),
    backgroundColor: "transparent",
    grid: { left: 8, right: 12, top: 18, bottom: 8, containLabel: true },
    tooltip: tooltipOptions(palette, (params) => {
      const items = Array.isArray(params) ? params : [params];
      const datum = items.find((item) => item?.data?.source);
      return datum ? formatTooltip(datum.data.source) : "";
    }),
    xAxis: {
      ...axisLine(palette),
      type: "time",
      min: rangeFrom,
      max: rangeTo,
      boundaryGap: false,
      splitNumber: 3,
      axisLabel: {
        ...axisLine(palette).axisLabel,
        formatter: (value) => formatAxis(Number(value)),
      },
      ...calendarAxisOptions(range, chartWidth, formatAxis, axisLine(palette).axisLabel),
    },
    yAxis: {
      ...axisLine(palette),
      type: "value",
      min: 0,
      splitNumber: 4,
      axisLabel: {
        ...axisLine(palette).axisLabel,
        formatter: (value) => formatValue(Number(value)),
      },
    },
    series: [{
      id: "usage-trend",
      name: metric,
      type: "line",
      data,
      encode: { x: 0, y: 1 },
      showSymbol: data.length <= 240,
      symbol: "circle",
      symbolSize: 7,
      sampling: "lttb",
      connectNulls: false,
      clip: true,
      lineStyle: {
        color: palette.accent,
        width: 3,
        cap: "round",
        join: "round",
        shadowColor: palette.accentShadow,
        shadowBlur: 8,
        shadowOffsetY: 4,
      },
      itemStyle: {
        color: palette.surfaceSolid,
        borderColor: palette.accent,
        borderWidth: 2,
      },
      areaStyle: {
        color: {
          type: "linear",
          x: 0,
          y: 0,
          x2: 0,
          y2: 1,
          colorStops: [
            { offset: 0, color: palette.accentArea },
            { offset: 1, color: palette.transparent },
          ],
        },
      },
      emphasis: {
        focus: "series",
        scale: 1.45,
        itemStyle: { borderWidth: 3, shadowBlur: 10, shadowColor: palette.accentShadow },
      },
      universalTransition: true,
    }],
  };
}

export function joinQuotaSegments(segments, valueSelector) {
  const data = [];
  let previousPoint = null;
  for (const segment of segments || []) {
    const firstPoint = segment[0];
    if (!firstPoint) continue;
    if (previousPoint) {
      const gapAt = Math.floor((Number(previousPoint.sampledAt) + Number(firstPoint.sampledAt)) / 2);
      data.push({ value: [gapAt * 1_000, null], source: null });
    }
    for (const point of segment) {
      const value = valueSelector(point);
      if (value == null || !Number.isFinite(Number(value))) continue;
      data.push({
        id: String(point.sampledAt),
        name: String(point.sampledAt),
        value: [Number(point.sampledAt) * 1_000, Number(value)],
        source: point,
      });
      previousPoint = point;
    }
  }
  return data;
}

export function dailyCalendarLayout(range, chartWidth = 1120) {
  const start = Date.parse(`${range?.[0] || ""}T00:00:00Z`);
  const end = Date.parse(`${range?.[1] || ""}T00:00:00Z`);
  const width = Number.isFinite(Number(chartWidth)) && Number(chartWidth) > 0
    ? Number(chartWidth)
    : 1120;
  const dayCount = Number.isFinite(start) && Number.isFinite(end) && end >= start
    ? Math.round((end - start) / DAY_MS) + 1
    : 1;
  const leadingDays = Number.isFinite(start) ? new Date(start).getUTCDay() : 0;
  const weekCount = Math.max(1, Math.ceil((leadingDays + dayCount) / 7));
  const showLabels = width >= DAILY_COMPACT_WIDTH;
  const labelSpace = showLabels ? DAILY_LABEL_SPACE : 0;
  const cellSize = Math.max(
    DAILY_MIN_CELL_SIZE,
    Math.min(DAILY_MAX_CELL_SIZE, Math.floor((width - labelSpace) / weekCount)),
  );
  const contentWidth = labelSpace + weekCount * cellSize;
  return {
    cellSize,
    weekCount,
    left: labelSpace + Math.max(0, Math.floor((width - contentWidth) / 2)),
    top: showLabels ? 28 : 8,
    height: (showLabels ? 28 : 8) + 7 * cellSize + 20,
    showLabels,
    borderWidth: cellSize >= 10 ? 2 : 1,
  };
}

export function buildQuotaOption({
  plots,
  range,
  amountRange,
  chartWidth = 840,
  palette,
  formatAxis,
  formatAmount,
  formatTooltip,
  percentAxisName,
  amountAxisName,
  ariaDescription,
  reducedMotion = false,
}) {
  const rangeFrom = Number(range.from) * 1_000;
  const rangeTo = Math.max(Number(range.to) * 1_000, rangeFrom + 1_000);
  const hasAmounts = plots.some((plot) => plot.axis === "amount");
  const pointCount = plots.reduce(
    (total, plot) => total + plot.segments.reduce((count, segment) => count + segment.length, 0),
    0,
  );
  const axes = axisLine(palette);
  const series = plots.map((plot) => {
    const data = joinQuotaSegments(plot.segments, plot.value)
      .map((datum) => ({ ...datum, axis: plot.axis }));
    return {
      id: plot.id,
      name: plot.name,
      type: "line",
      yAxisIndex: plot.axis === "amount" ? 1 : 0,
      data,
      encode: { x: 0, y: 1 },
      showSymbol: false,
      symbol: "circle",
      symbolSize: 6,
      sampling: "lttb",
      connectNulls: false,
      clip: true,
      lineStyle: {
        color: plot.color,
        width: 2.5,
        type: plot.axis === "amount" ? "dashed" : "solid",
        cap: "round",
        join: "round",
      },
      itemStyle: { color: plot.color, borderColor: palette.surfaceSolid, borderWidth: 1.5 },
      emphasis: { focus: "series", scale: 1.5 },
      universalTransition: true,
    };
  });
  return {
    ...animationOptions(reducedMotion, pointCount),
    aria: ariaOptions(ariaDescription),
    backgroundColor: "transparent",
    color: plots.map((plot) => plot.color),
    grid: {
      left: 8,
      right: hasAmounts ? 8 : 12,
      top: 34,
      bottom: plots.length > 1 ? 50 : 22,
      containLabel: true,
    },
    tooltip: {
      ...tooltipOptions(palette, formatTooltip),
      renderMode: "html", appendTo: "body", confine: false, enterable: true,
      transitionDuration: 0, className: "echarts-tooltip quota-tooltip",
      position: (point, params, dom, rect, size) => dailyTooltipPosition(point, params, dom, rect, size, "quotaChart"),
      axisPointer: { axis: "x", type: "line" },
    },
    legend: {
      show: plots.length > 1,
      type: "scroll",
      left: 4,
      right: 4,
      bottom: 0,
      itemWidth: 18,
      itemHeight: 8,
      itemGap: 14,
      pageIconColor: palette.accent,
      pageIconInactiveColor: palette.faint,
      pageTextStyle: { color: palette.muted, fontFamily: FONT_FAMILY, fontSize: 10 },
      textStyle: { color: palette.muted, fontFamily: FONT_FAMILY, fontSize: 10 },
    },
    xAxis: {
      ...axes,
      type: "time",
      min: rangeFrom,
      max: rangeTo,
      boundaryGap: false,
      splitNumber: 3,
      axisLabel: { ...axes.axisLabel, formatter: (value) => formatAxis(Number(value)) },
      ...calendarAxisOptions(range, chartWidth, formatAxis, axes.axisLabel),
    },
    yAxis: [
      {
        ...axes,
        type: "value",
        name: percentAxisName,
        nameLocation: "end",
        nameTextStyle: { color: palette.muted, fontFamily: FONT_FAMILY, fontSize: 11, fontWeight: 700 },
        min: 0,
        max: 100,
        interval: 25,
        axisLabel: { ...axes.axisLabel, formatter: (value) => `${Number(value)}%` },
      },
      {
        ...axes,
        show: hasAmounts,
        type: "value",
        name: amountAxisName,
        nameLocation: "end",
        nameTextStyle: { color: palette.muted, fontFamily: FONT_FAMILY, fontSize: 11, fontWeight: 700 },
        position: "right",
        min: amountRange.minimum,
        max: amountRange.maximum,
        splitNumber: 4,
        axisLabel: { ...axes.axisLabel, formatter: (value) => formatAmount(Number(value)) },
        splitLine: { show: false },
      },
    ],
    series,
  };
}

export function buildDailyOption({
  points,
  range,
  chartWidth = 1120,
  palette,
  colors,
  dayNames,
  monthNames,
  formatTooltip,
  ariaDescription,
  reducedMotion = false,
}) {
  const layout = dailyCalendarLayout(range, chartWidth);
  const data = (points || []).map(({ date, value, level, source }) => ({
    id: date,
    name: date,
    value: [date, Number(value || 0), Number(level || 0)],
    source,
  }));
  return {
    ...animationOptions(reducedMotion, data.length),
    aria: ariaOptions(ariaDescription),
    backgroundColor: "transparent",
    tooltip: {
      ...tooltipOptions(palette, formatTooltip, "item"),
      renderMode: "html",
      appendTo: "body",
      enterable: true,
      transitionDuration: 0,
      confine: false,
      className: "echarts-tooltip daily-tooltip",
      textStyle: { color: palette.text, fontFamily: FONT_FAMILY, fontSize: 14, lineHeight: 22 },
      position: dailyTooltipPosition,
    },
    visualMap: {
      show: false,
      type: "piecewise",
      dimension: 2,
      pieces: colors.map((color, value) => ({ value, color })),
    },
    calendar: {
      top: layout.top,
      left: layout.left,
      range,
      orient: "horizontal",
      cellSize: [layout.cellSize, layout.cellSize],
      splitLine: { show: false },
      itemStyle: {
        color: palette.surfaceRaised,
        borderColor: palette.surfaceSolid,
        borderWidth: layout.borderWidth,
        borderRadius: 3,
      },
      yearLabel: { show: false },
      dayLabel: {
        show: layout.showLabels,
        firstDay: 0,
        margin: 7,
        color: palette.muted,
        fontFamily: FONT_FAMILY,
        fontSize: 12,
        nameMap: dayNames,
      },
      monthLabel: {
        show: layout.showLabels,
        margin: 8,
        color: palette.muted,
        fontFamily: FONT_FAMILY,
        fontSize: 12,
        nameMap: monthNames,
      },
    },
    series: [{
      id: "daily-usage",
      name: ariaDescription,
      type: "heatmap",
      coordinateSystem: "calendar",
      data,
      encode: { value: 1 },
      itemStyle: {
        borderColor: palette.surfaceSolid,
        borderWidth: layout.borderWidth,
        borderRadius: 3,
      },
      emphasis: {
        itemStyle: {
          borderColor: palette.accent,
          borderWidth: layout.borderWidth,
          shadowBlur: 9,
          shadowColor: palette.accentShadow,
        },
      },
      universalTransition: true,
    }],
  };
}

export function dailyTooltipPosition(point, params, dom, rect, size, chartId = "dailyHeatmap") {
  const bounds = dom.ownerDocument.documentElement;
  const chart = dom.ownerDocument.getElementById(chartId);
  const origin = chart?.getBoundingClientRect() || { left: 0, top: 0 };
  const padding = 12;
  const width = size.contentSize[0];
  const height = size.contentSize[1];
  let x = origin.left + point[0] + padding;
  let y = origin.top + point[1] + padding;
  if (x + width > bounds.clientWidth - padding) x = origin.left + point[0] - width - padding;
  if (y + height > bounds.clientHeight - padding) y = origin.top + point[1] - height - padding;
  x = Math.max(padding, Math.min(x, bounds.clientWidth - width - padding));
  y = Math.max(padding, Math.min(y, bounds.clientHeight - height - padding));
  return [x - origin.left, y - origin.top];
}

export function usageTooltipMarkup(point, title, lines, formatInteger) {
  return tooltipMarkup(title,
    [["", `Total: ${formatInteger(Number(point.realTotalTokens || 0))}`], ...lines]);
}

// Generate all day ticks first; explicitly thin labels at a regular calendar stride.
export function calendarAxisOptions(range, width, formatAxis, axisLabel = {}) {
  const from = Number(range.from) * 1000, to = Number(range.to) * 1000;
  if (to - from <= 2 * DAY_MS) return {};
  const first = new Date(from);
  first.setHours(0, 0, 0, 0);
  if (first.getTime() < from) first.setDate(first.getDate() + 1);
  const end = new Date(to);
  const ordinal = (date) => Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / DAY_MS;
  const count = Math.max(1, ordinal(end) - ordinal(first) + 1);
  const slots = Math.max(2, Math.floor(Number(width || 840) / 90));
  const required = Math.ceil(count / slots);
  const step = [1, 2, 3, 7, 14, 30, 60, 90, 180].find((n) => n >= required) || required;
  const labels = new Set();
  for (const date = new Date(first); date.getTime() <= to; date.setDate(date.getDate() + step)) labels.add(date.getTime());
  return {
    minInterval: DAY_MS, maxInterval: DAY_MS, splitNumber: Math.ceil((to - from) / DAY_MS),
    axisLabel: { ...axisLabel, hideOverlap: false, showMinLabel: true, showMaxLabel: true,
      formatter: (value) => labels.has(Number(value)) ? formatAxis(Number(value)) : "" },
  };
}
