import { createFormatters, resolveLocale, translate } from "./i18n.js";
import {
  DAY_SECONDS,
  addLocalDaysMs,
  bucketDisplayLabel,
  calendarDays,
  dateInputValue,
  defaultCustomRange,
  parseBucketValue,
  parseDateTimeParts,
  resolvePresetRange,
  sameLocalDay,
  setDateKeepTime,
  splitBucketValue,
  startOfLocalDayMs,
  timeInputValue,
} from "./range.js";

const $ = (id) => document.getElementById(id);
const svgNs = "http://www.w3.org/2000/svg";
const localeStorageKey = "cc-switch-telemetry.locale";
const themeStorageKey = "cc-switch-telemetry.theme";

function storedLocale() {
  try {
    return localStorage.getItem(localeStorageKey);
  } catch {
    return null;
  }
}

let locale = resolveLocale(
  storedLocale(),
  navigator.languages ?? [navigator.language].filter(Boolean),
);
let formatters = createFormatters(locale);
const t = (key, variables = {}) => translate(locale, key, variables);

function storedTheme() {
  try {
    return localStorage.getItem(themeStorageKey);
  } catch {
    return null;
  }
}

let theme = storedTheme() === "light" ? "light" : "dark";

const elements = {
  statusDot: $("statusDot"),
  statusText: $("statusText"),
  updatedAt: $("updatedAt"),
  themeToggle: $("themeToggle"),
  themeToggleLabel: $("themeToggleLabel"),
  languageToggle: $("languageToggle"),
  refreshButton: $("refreshButton"),
  errorBanner: $("errorBanner"),
  brandLockup: $("brandLockup"),
  brandMark: $("brandMark"),
  rangePickerTrigger: $("rangePickerTrigger"),
  rangePickerLabel: $("rangePickerLabel"),
  rangePickerDialog: $("rangePickerDialog"),
  rangePickerForm: $("rangePickerForm"),
  rangePresetOptions: $("rangePresetOptions"),
  customRangeError: $("customRangeError"),
  closeRangePicker: $("closeRangePicker"),
  cancelRange: $("cancelRange"),
  customFromDate: $("customFromDate"),
  customFromTime: $("customFromTime"),
  customToDate: $("customToDate"),
  customToTime: $("customToTime"),
  applyRange: $("applyRange"),
  calendarMonthLabel: $("calendarMonthLabel"),
  calendarWeekdays: $("calendarWeekdays"),
  calendarDays: $("calendarDays"),
  previousCalendarMonth: $("previousCalendarMonth"),
  nextCalendarMonth: $("nextCalendarMonth"),
  trendBucketTrigger: $("trendBucketTrigger"),
  trendBucketLabel: $("trendBucketLabel"),
  bucketPickerDialog: $("bucketPickerDialog"),
  bucketPickerForm: $("bucketPickerForm"),
  bucketPresetOptions: $("bucketPresetOptions"),
  closeBucketPicker: $("closeBucketPicker"),
  customBucketAmount: $("customBucketAmount"),
  customBucketUnit: $("customBucketUnit"),
  applyCustomBucket: $("applyCustomBucket"),
  customBucketError: $("customBucketError"),
  nodeFilter: $("nodeFilter"),
  appFilter: $("appFilter"),
  providerFilter: $("providerFilter"),
  modelFilter: $("modelFilter"),
  sourceFilter: $("sourceFilter"),
  trendMetric: $("trendMetric"),
  resolvedBucket: $("resolvedBucket"),
  trendChart: $("trendChart"),
  trendEmpty: $("trendEmpty"),
  trendTooltip: $("trendTooltip"),
  coverageText: $("coverageText"),
  kpiInputTotal: $("kpiInputTotal"),
  kpiOutputTotal: $("kpiOutputTotal"),
  kpiFreshTokens: $("kpiFreshTokens"),
  kpiCreationTokens: $("kpiCreationTokens"),
  kpiCachedTokens: $("kpiCachedTokens"),
  tokenComposition: $("tokenComposition"),
  freshTokenBar: $("freshTokenBar"),
  creationTokenBar: $("creationTokenBar"),
  cachedTokenBar: $("cachedTokenBar"),
  kpiCostTopModels: $("kpiCostTopModels"),
  breakdownTabs: $("breakdownTabs"),
  breakdownRows: $("breakdownRows"),
  breakdownEmpty: $("breakdownEmpty"),
  eventRows: $("eventRows"),
  eventsEmpty: $("eventsEmpty"),
  loadMore: $("loadMore"),
  dailyMetric: $("dailyMetric"),
  dailyEmpty: $("dailyEmpty"),
  dailyPanel: $("dailyPanel"),
  dailyHeatmap: $("dailyHeatmap"),
  dailyTooltip: $("dailyTooltip"),
};

const state = {
  overview: null,
  daily: null,
  breakdownDimension: "nodes",
  eventCursor: null,
  requestController: null,
  eventsLoading: false,
  eventsGeneration: 0,
  events: [],
  lastError: null,
  updatedAt: null,
  rangePreset: "24h",
  customRange: defaultCustomRange(),
  rangeDraft: null,
  rangeCalendarMonth: new Date(new Date().getFullYear(), new Date().getMonth(), 1),
  activeRangeField: "start",
  trendBucket: "auto",
  connection: { status: "", key: "status.connecting" },
};

function selectedRange() {
  const range = resolvePresetRange(state.rangePreset, Date.now(), state.customRange);
  if (!Number.isFinite(range.from) || !Number.isFinite(range.to) || range.from >= range.to) {
    const error = new Error(t("error.invalidRange"));
    error.translationKey = "error.invalidRange";
    throw error;
  }
  return range;
}

function baseParams(includeFilters = true) {
  const range = selectedRange();
  const params = new URLSearchParams({
    from: String(range.from),
    to: String(range.to),
    tz_offset_minutes: String(-new Date().getTimezoneOffset()),
  });
  if (includeFilters) {
    const filters = [
      ["node_id", elements.nodeFilter.value],
      ["app_type", elements.appFilter.value],
      ["provider_id", elements.providerFilter.value],
      ["model", elements.modelFilter.value],
      ["data_source", elements.sourceFilter.value],
    ];
    for (const [key, value] of filters) {
      if (value) params.set(key, value);
    }
  }
  return params;
}

function dailyParams(includeFilters = true) {
  const now = Date.now();
  const end = startOfLocalDayMs(now) + DAY_SECONDS * 1000;
  const start = addLocalDaysMs(end, -365);
  const params = new URLSearchParams({
    from: String(Math.floor(start / 1000)),
    to: String(Math.floor(end / 1000)),
    tz_offset_minutes: String(-new Date().getTimezoneOffset()),
  });
  if (includeFilters) {
    for (const [key, value] of [
      ["node_id", elements.nodeFilter.value],
      ["app_type", elements.appFilter.value],
      ["provider_id", elements.providerFilter.value],
      ["model", elements.modelFilter.value],
      ["data_source", elements.sourceFilter.value],
    ]) {
      if (value) params.set(key, value);
    }
  }
  return params;
}

async function fetchJson(url, signal) {
  const response = await fetch(url, { signal, headers: { Accept: "application/json" } });
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try {
      const body = await response.json();
      if (body.message) message = body.message;
    } catch {
      // Keep the HTTP status when the response is not JSON.
    }
    throw new Error(message);
  }
  return response.json();
}

function setConnection(status, key) {
  state.connection = { status, key };
  elements.statusDot.className = `status-dot ${status}`;
  elements.statusText.textContent = t(key);
}

function showError(error) {
  state.lastError = error;
  const message = error.translationKey ? t(error.translationKey) : error.message;
  elements.errorBanner.textContent = t("error.refresh", { message });
  elements.errorBanner.hidden = false;
  setConnection("error", "status.error");
}

function clearError() {
  state.lastError = null;
  elements.errorBanner.hidden = true;
  elements.errorBanner.textContent = "";
}

function setSelectOptions(select, values, allLabel) {
  const previous = select.value;
  select.replaceChildren();
  const all = document.createElement("option");
  all.value = "";
  all.textContent = allLabel;
  select.append(all);
  for (const item of values) {
    const value = typeof item === "string" ? item : item?.value;
    const label = typeof item === "string" ? item : item?.label || value;
    if (!value) continue;
    const option = document.createElement("option");
    option.value = value;
    option.textContent = label;
    select.append(option);
  }
  if ([...select.options].some((option) => option.value === previous)) {
    select.value = previous;
  }
}

async function refreshFilters(signal) {
  const params = baseParams(false);
  const filters = await fetchJson(`/v1/dashboard/filters?${params}`, signal);
  setSelectOptions(elements.nodeFilter, filters.nodes, t("filters.allNodes"));
  setSelectOptions(elements.appFilter, filters.apps, t("filters.allApps"));
  setSelectOptions(elements.providerFilter, filters.providers, t("filters.allProviders"));
  setSelectOptions(elements.modelFilter, filters.models, t("filters.allModels"));
  setSelectOptions(elements.sourceFilter, filters.dataSources, t("filters.allSources"));
}

function formatTokens(value) {
  return formatters.compactNumber.format(value || 0);
}

function formatPercent(value, ratio = false) {
  const percent = ratio ? (value || 0) * 100 : (value || 0);
  return `${percent.toFixed(1)}%`;
}

function formatLatency(value) {
  if ((value || 0) >= 1000) return `${(value / 1000).toFixed(2)} s`;
  return `${Math.round(value || 0)} ms`;
}

function animateMetric(element, target, formatter) {
  const start = Number(element.dataset.numericValue ?? 0);
  const duration = 460;
  const begin = performance.now();
  if (element.animationFrame) cancelAnimationFrame(element.animationFrame);
  element.dataset.numericValue = String(target);
  element.classList.remove("metric-value-updated");
  void element.offsetWidth;
  element.classList.add("metric-value-updated");
  const tick = (now) => {
    const progress = Math.min(1, (now - begin) / duration);
    const eased = 1 - (1 - progress) ** 3;
    element.textContent = formatter(start + (target - start) * eased);
    if (progress < 1) {
      element.animationFrame = requestAnimationFrame(tick);
    } else {
      element.textContent = formatter(target);
      element.animationFrame = null;
    }
  };
  element.animationFrame = requestAnimationFrame(tick);
}

function pulseMetric(element) {
  element.classList.remove("metric-value-updated");
  void element.offsetWidth;
  element.classList.add("metric-value-updated");
}

function setTokenBarWidth(element, value, total) {
  element.style.width = total > 0 ? `${Math.max(0, value) / total * 100}%` : "0%";
}

function animateTokenParts(summary) {
  const fresh = Number(summary.freshInputTokens || 0);
  const creation = Number(summary.cacheCreationTokens || 0);
  const cached = Number(summary.cacheReadTokens || 0);
  const output = Number(summary.outputTokens || 0);
  const input = fresh + creation + cached;
  animateMetric(elements.kpiInputTotal, input, formatTokens);
  animateMetric(elements.kpiOutputTotal, output, formatTokens);
  animateMetric(elements.kpiFreshTokens, fresh, formatTokens);
  animateMetric(elements.kpiCreationTokens, creation, formatTokens);
  animateMetric(elements.kpiCachedTokens, cached, formatTokens);
  setTokenBarWidth(elements.freshTokenBar, fresh, input);
  setTokenBarWidth(elements.creationTokenBar, creation, input);
  setTokenBarWidth(elements.cachedTokenBar, cached, input);
  const tokenCompositionLabel = t("kpi.tokenCompositionAria", {
    fresh: formatTokens(fresh),
    creation: formatTokens(creation),
    cached: formatTokens(cached),
  });
  elements.tokenComposition.setAttribute("aria-label", tokenCompositionLabel);
  elements.tokenComposition.setAttribute("title", tokenCompositionLabel);
}

function renderSummary(summary) {
  animateMetric($("kpiRequests"), summary.totalRequests, (value) => formatters.integerNumber.format(value));
  animateMetric($("kpiTokens"), summary.realTotalTokens, formatTokens);
  animateMetric($("kpiCost"), summary.totalCostUsd, (value) => formatters.moneyNumber.format(value));
  animateMetric($("kpiSuccessRate"), summary.successRate, (value) => formatPercent(value));
  animateMetric($("kpiCacheRate"), summary.cacheHitRate * 100, (value) => formatPercent(value));
  animateMetric($("kpiLatency"), summary.avgLatencyMs, formatLatency);
  $("kpiSuccessCount").textContent = t("kpi.successCount", {
    count: formatters.integerNumber.format(summary.successfulRequests),
  });
  pulseMetric($("kpiSuccessCount"));
  animateTokenParts(summary);
}

function renderCostTopModels(items) {
  elements.kpiCostTopModels.replaceChildren();
  const topModels = [...(items || [])]
    .filter((item) => Number(item.totalCostUsd || 0) > 0)
    .sort((left, right) => Number(right.totalCostUsd || 0) - Number(left.totalCostUsd || 0))
    .slice(0, 3);
  for (const item of topModels) {
    const row = document.createElement("div");
    row.className = "cost-model-row";
    const name = document.createElement("span");
    name.className = "cost-model-name";
    name.textContent = item.key || t("common.unknown");
    name.title = name.textContent;
    const amount = document.createElement("strong");
    amount.className = "cost-model-amount";
    amount.textContent = formatters.moneyNumber.format(item.totalCostUsd);
    row.append(name, amount);
    elements.kpiCostTopModels.append(row);
  }
  if (!topModels.length) {
    const empty = document.createElement("span");
    empty.className = "cost-model-name";
    empty.textContent = "—";
    elements.kpiCostTopModels.append(empty);
  }
}

function createSvg(name, attributes = {}, text = "") {
  const element = document.createElementNS(svgNs, name);
  for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, String(value));
  if (text) element.textContent = text;
  return element;
}

function trendValue(point, metric) {
  return Number(point[metric] || 0);
}

function trendValueLabel(value, metric) {
  if (metric === "totalCostUsd") return formatters.moneyNumber.format(value);
  if (metric === "avgLatencyMs") return formatLatency(value);
  return formatters.compactNumber.format(value);
}

function formatTrendAxis(timestamp, spanSeconds) {
  const options = spanSeconds <= 2 * DAY_SECONDS
    ? { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }
    : spanSeconds <= 14 * DAY_SECONDS
      ? { month: "2-digit", day: "2-digit" }
      : { year: "numeric", month: "2-digit", day: "2-digit" };
  return new Intl.DateTimeFormat(locale, options)
    .format(timestamp * 1000)
    .replaceAll("/", "-");
}

function renderTrend() {
  const points = state.overview?.trend || [];
  elements.trendTooltip.hidden = true;
  elements.trendChart.replaceChildren();
  elements.trendEmpty.hidden = points.length > 0;
  elements.trendChart.hidden = points.length === 0;
  if (state.overview?.range?.bucket) {
    elements.resolvedBucket.textContent = t("trend.resolvedBucket", {
      bucket: state.overview.range.bucket,
    });
  }
  if (!points.length) return;

  const metric = elements.trendMetric.value;
  const width = 900;
  const height = 280;
  const padding = { left: 64, right: 22, top: 20, bottom: 42 };
  const plotWidth = width - padding.left - padding.right;
  const plotHeight = height - padding.top - padding.bottom;
  const values = points.map((point) => trendValue(point, metric));
  const maximum = Math.max(...values, 1);
  const rangeFrom = Number(state.overview.range.from);
  const rangeTo = Math.max(Number(state.overview.range.to), rangeFrom + 1);
  const rangeSpan = rangeTo - rangeFrom;
  const x = (timestamp) => padding.left + Math.max(0, Math.min(1, (timestamp - rangeFrom) / rangeSpan)) * plotWidth;
  const y = (value) => padding.top + plotHeight - value / maximum * plotHeight;

  for (let index = 0; index <= 4; index += 1) {
    const gridY = padding.top + index * plotHeight / 4;
    elements.trendChart.append(createSvg("line", { x1: padding.left, x2: width - padding.right, y1: gridY, y2: gridY, class: "grid" }));
    const labelValue = maximum * (1 - index / 4);
    elements.trendChart.append(createSvg("text", { x: padding.left - 10, y: gridY + 4, "text-anchor": "end", class: "axis-label" }, trendValueLabel(labelValue, metric)));
  }

  const coordinates = points.map((point, index) => [x(point.bucketStart), y(values[index])]);
  const linePath = coordinates.map(([cx, cy], index) => `${index ? "L" : "M"} ${cx} ${cy}`).join(" ");
  const areaPath = `${linePath} L ${coordinates.at(-1)[0]} ${padding.top + plotHeight} L ${coordinates[0][0]} ${padding.top + plotHeight} Z`;
  elements.trendChart.append(createSvg("path", { d: areaPath, class: "area" }));
  elements.trendChart.append(createSvg("path", { d: linePath, class: "line", pathLength: 1 }));

  points.forEach((point, index) => {
    const [cx, cy] = coordinates[index];
    const circle = createSvg("circle", { cx, cy, r: 4, class: "point" });
    circle.addEventListener("pointerenter", (event) => showTrendTooltip(point, event));
    circle.addEventListener("pointermove", (event) => showTrendTooltip(point, event));
    circle.addEventListener("pointerleave", () => { elements.trendTooltip.hidden = true; });
    elements.trendChart.append(circle);
  });

  const axisLabels = [rangeFrom, rangeFrom + rangeSpan / 2, rangeTo];
  axisLabels.forEach((timestamp, index) => {
    elements.trendChart.append(createSvg("text", {
      x: x(timestamp),
      y: height - 13,
      "text-anchor": index === 0 ? "start" : index === axisLabels.length - 1 ? "end" : "middle",
      class: "axis-label",
    }, formatTrendAxis(timestamp, rangeSpan)));
  });
}

function showUsageTooltip(tooltip, panel, point, titleText, event) {
  tooltip.replaceChildren();
  const title = document.createElement("strong");
  title.textContent = titleText;
  tooltip.append(title);
  const lines = [
    ["trend.tooltipInput", formatTokens(point.inputTokens)],
    ["trend.tooltipFreshInput", formatTokens(point.freshInputTokens)],
    ["trend.tooltipCacheCreation", formatTokens(point.cacheCreationTokens)],
    ["trend.tooltipCacheRead", formatTokens(point.cacheReadTokens)],
    ["trend.tooltipOutput", formatTokens(point.outputTokens)],
    ["trend.tooltipRequests", formatters.integerNumber.format(point.totalRequests)],
    ["trend.tooltipSuccess", formatPercent(point.successRate)],
    ["trend.tooltipCost", formatters.moneyNumber.format(point.totalCostUsd)],
    ["trend.tooltipLatency", formatLatency(point.avgLatencyMs)],
  ];
  for (const [key, value] of lines) {
    const line = document.createElement("span");
    line.textContent = t(key, { value });
    tooltip.append(line);
  }
  tooltip.hidden = false;
  const panelRect = panel.getBoundingClientRect();
  const targetX = event.clientX - panelRect.left + 14;
  const targetY = event.clientY - panelRect.top - tooltip.offsetHeight - 14;
  const maxLeft = panel.clientWidth - tooltip.offsetWidth - 12;
  tooltip.style.left = `${Math.max(12, Math.min(targetX, maxLeft))}px`;
  tooltip.style.top = `${Math.max(12, targetY)}px`;
}

function showTrendTooltip(point, event) {
  showUsageTooltip(
    elements.trendTooltip,
    elements.trendChart.parentElement,
    point,
    formatters.dateTime.format(point.bucketStart * 1000),
    event,
  );
}

function showDailyTooltip(point, date, event) {
  showUsageTooltip(
    elements.dailyTooltip,
    elements.dailyPanel,
    point,
    new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(date),
    event,
  );
}

function appendCell(row, value, className = "") {
  const cell = document.createElement("td");
  cell.textContent = value;
  if (className) cell.className = className;
  row.append(cell);
  return cell;
}

function renderBreakdown() {
  const items = state.overview?.breakdowns?.[state.breakdownDimension] || [];
  elements.breakdownRows.replaceChildren();
  elements.breakdownEmpty.hidden = items.length > 0;
  if (!items.length) return;
  for (const item of items) {
    const row = document.createElement("tr");
    appendCell(row, item.label || item.key || t("common.unknown"));
    appendCell(row, formatters.integerNumber.format(item.totalRequests), "table-value");
    appendCell(row, formatTokens(item.realTotalTokens), "table-value");
    appendCell(row, formatPercent(item.successRate), "table-value");
    appendCell(row, formatters.moneyNumber.format(item.totalCostUsd), "table-value");
    elements.breakdownRows.append(row);
  }
}

function renderCoverage(coverage) {
  if (!coverage.firstEventAt || !coverage.lastEventAt) {
    elements.coverageText.textContent = t("coverage.empty");
    return;
  }
  elements.coverageText.textContent = t("coverage.range", {
    from: formatters.dateTime.format(coverage.firstEventAt * 1000),
    to: formatters.dateTime.format(coverage.lastEventAt * 1000),
  });
}

function renderOverview(overview) {
  state.overview = overview;
  renderSummary(overview.summary);
  renderCostTopModels(overview.breakdowns?.models);
  renderTrend();
  renderBreakdown();
  renderCoverage(overview.coverage);
}

function dailyMetricValue(point, metric) {
  return Number(point?.[metric] || 0);
}

function dailyMetricLabel(value, metric) {
  if (metric === "totalCostUsd") return formatters.moneyNumber.format(value);
  if (metric === "totalRequests") return formatters.integerNumber.format(value);
  return formatTokens(value);
}

function localDateKey(date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

let brandMarkSyncQueued = false;

function syncBrandMarkSize() {
  if (brandMarkSyncQueued) return;
  brandMarkSyncQueued = true;
  requestAnimationFrame(() => {
    brandMarkSyncQueued = false;
    const height = Math.round(elements.brandMark.getBoundingClientRect().height);
    if (height > 0) elements.brandMark.style.setProperty("--brand-mark-size", `${height}px`);
  });
}

function renderDaily() {
  const points = state.daily?.days || [];
  const metric = elements.dailyMetric.value;
  elements.dailyTooltip.hidden = true;
  elements.dailyHeatmap.replaceChildren();
  elements.dailyEmpty.hidden = points.length > 0;
  elements.dailyHeatmap.hidden = points.length === 0;
  if (!points.length) return;

  const byDay = new Map();
  for (const point of points) {
    byDay.set(localDateKey(new Date(point.bucketStart * 1000)), point);
  }
  const first = new Date(points[0].bucketStart * 1000);
  const last = new Date(points.at(-1).bucketStart * 1000);
  first.setHours(0, 0, 0, 0);
  last.setHours(0, 0, 0, 0);
  const gridStart = new Date(first);
  gridStart.setDate(gridStart.getDate() - gridStart.getDay());
  const gridEnd = new Date(last);
  gridEnd.setDate(gridEnd.getDate() + (6 - gridEnd.getDay()));
  const weekCount = Math.floor((gridEnd - gridStart) / (7 * DAY_SECONDS * 1000)) + 1;
  elements.dailyHeatmap.style.gridTemplateColumns = `30px repeat(${weekCount}, 13px)`;

  const values = points.map((point) => dailyMetricValue(point, metric)).filter((value) => value > 0).sort((a, b) => a - b);
  const maxValue = values.at(-1) || 0;
  const levelFor = (value) => {
    if (value <= 0 || maxValue <= 0) return 0;
    if (value === maxValue) return 4;
    const rank = values.findIndex((item) => item >= value);
    return Math.max(1, Math.min(4, Math.ceil((rank + 1) / values.length * 4)));
  };
  const weekdayFormatter = new Intl.DateTimeFormat(locale, { weekday: "short" });
  for (let weekday = 0; weekday < 7; weekday += 1) {
    const label = document.createElement("span");
    label.className = "daily-weekday";
    label.style.gridRow = String(weekday + 2);
    label.textContent = weekdayFormatter.format(new Date(2024, 0, weekday));
    elements.dailyHeatmap.append(label);
  }
  const monthFormatter = new Intl.DateTimeFormat(locale, { month: "short" });
  let previousMonth = -1;
  for (let week = 0; week < weekCount; week += 1) {
    const weekDate = new Date(gridStart);
    weekDate.setDate(gridStart.getDate() + week * 7);
    if (weekDate.getMonth() !== previousMonth) {
      const label = document.createElement("span");
      label.className = "daily-month";
      label.style.gridColumn = String(week + 2);
      label.style.gridRow = "1";
      label.textContent = monthFormatter.format(weekDate);
      elements.dailyHeatmap.append(label);
      previousMonth = weekDate.getMonth();
    }
    for (let weekday = 0; weekday < 7; weekday += 1) {
      const date = new Date(weekDate);
      date.setDate(weekDate.getDate() + weekday);
      const tile = document.createElement("span");
      tile.className = "daily-tile";
      tile.style.gridColumn = String(week + 2);
      tile.style.gridRow = String(weekday + 2);
      const point = byDay.get(localDateKey(date));
      const value = dailyMetricValue(point, metric);
      tile.classList.add(`daily-level-${levelFor(value)}`);
      if (point) {
        const dateLabel = new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(date);
        const tooltip = `${dateLabel}: ${dailyMetricLabel(value, metric)}`;
        tile.setAttribute("aria-label", tooltip);
        tile.setAttribute("role", "gridcell");
        tile.addEventListener("pointerenter", (event) => showDailyTooltip(point, date, event));
        tile.addEventListener("pointermove", (event) => showDailyTooltip(point, date, event));
        tile.addEventListener("pointerleave", () => { elements.dailyTooltip.hidden = true; });
      } else {
        tile.classList.add("empty");
        tile.setAttribute("aria-hidden", "true");
      }
      elements.dailyHeatmap.append(tile);
    }
  }
}

function eventStatus(statusCode) {
  if (statusCode >= 200 && statusCode < 300) return [t("events.success"), "status-pill"];
  return [String(statusCode || t("events.failure")), "status-pill failed"];
}

function renderEventRows(items, append) {
  if (!append) elements.eventRows.replaceChildren();
  for (const item of items) {
    const row = document.createElement("tr");
    row.title = t("events.requestId", { id: item.requestId });
    appendCell(row, formatters.dateTime.format(item.createdAt * 1000));
    appendCell(row, item.nodeId || "—");
    appendCell(row, item.appType || "—");
    appendCell(row, `${item.providerName || item.providerId || "—"} / ${item.model || item.requestModel || "—"}`);
    appendCell(row, formatTokens(item.realTotalTokens), "table-value");
    appendCell(row, formatTokens(item.cacheReadTokens), "table-value");
    appendCell(row, formatters.moneyNumber.format(item.totalCostUsd), "table-value");
    appendCell(row, formatLatency(item.latencyMs), "table-value");
    const [label, className] = eventStatus(item.statusCode);
    const statusCell = appendCell(row, "");
    const pill = document.createElement("span");
    pill.className = className;
    pill.textContent = label;
    statusCell.append(pill);
    appendCell(row, item.dataSource || "—");
    elements.eventRows.append(row);
  }
}

async function loadEvents({ append = false, signal = undefined } = {}) {
  if (append && state.eventsLoading) return;
  const generation = ++state.eventsGeneration;
  state.eventsLoading = true;
  elements.loadMore.disabled = true;
  try {
    const params = baseParams(true);
    params.set("limit", "50");
    if (append && state.eventCursor) {
      params.set("before_created_at", String(state.eventCursor.beforeCreatedAt));
      params.set("before_event_id", state.eventCursor.beforeEventId);
    }
    const response = await fetchJson(`/v1/dashboard/events?${params}`, signal);
    if (generation !== state.eventsGeneration) return;
    state.events = append ? [...state.events, ...response.items] : response.items;
    renderEventRows(state.events, false);
    state.eventCursor = response.nextCursor;
    elements.eventsEmpty.hidden = state.events.length > 0;
    elements.loadMore.hidden = !state.eventCursor;
  } finally {
    if (generation === state.eventsGeneration) {
      state.eventsLoading = false;
      elements.loadMore.disabled = false;
    }
  }
}

async function refreshAll({ reloadFilters = false } = {}) {
  if (state.requestController) state.requestController.abort();
  const controller = new AbortController();
  state.requestController = controller;
  elements.refreshButton.disabled = true;
  setConnection("", "status.refreshing");
  try {
    if (reloadFilters) await refreshFilters(controller.signal);
    const params = baseParams(true);
    params.set("bucket", state.trendBucket);
    const daily = dailyParams(true);
    const [overview, dailyResponse] = await Promise.all([
      fetchJson(`/v1/dashboard/overview?${params}`, controller.signal),
      fetchJson(`/v1/dashboard/daily?${daily}`, controller.signal),
      loadEvents({ append: false, signal: controller.signal }),
    ]);
    renderOverview(overview);
    state.daily = dailyResponse;
    renderDaily();
    clearError();
    setConnection("online", "status.online");
    state.updatedAt = Date.now();
    elements.updatedAt.textContent = t("status.updatedAt", {
      time: formatters.dateTime.format(state.updatedAt),
    });
  } catch (error) {
    if (error.name !== "AbortError") showError(error);
  } finally {
    if (state.requestController === controller) {
      state.requestController = null;
      elements.refreshButton.disabled = false;
    }
  }
}

function dialogIsOpen(dialog) {
  return dialog.open || dialog.hasAttribute("open");
}

function positionDialog(dialog, trigger) {
  if (!dialogIsOpen(dialog) || !trigger) return;
  const viewportPadding = 12;
  const triggerRect = trigger.getBoundingClientRect();
  const top = Math.max(viewportPadding, triggerRect.bottom + 8);
  dialog.style.setProperty("--picker-top", `${top}px`);
  dialog.style.setProperty("--picker-max-height", `${Math.max(0, window.innerHeight - top - viewportPadding)}px`);
  const dialogWidth = dialog.getBoundingClientRect().width;
  const left = Math.max(viewportPadding, Math.min(triggerRect.left, window.innerWidth - dialogWidth - viewportPadding));
  dialog.style.setProperty("--picker-left", `${left}px`);
}

function positionOpenPickers() {
  positionDialog(elements.rangePickerDialog, elements.rangePickerTrigger);
  positionDialog(elements.bucketPickerDialog, elements.trendBucketTrigger);
}

function openDialog(dialog, trigger) {
  if (!dialogIsOpen(dialog)) {
    if (typeof dialog.show === "function") dialog.show();
    else dialog.setAttribute("open", "");
  }
  trigger?.setAttribute("aria-expanded", "true");
  positionDialog(dialog, trigger);
}

function closeDialog(dialog, trigger) {
  if (typeof dialog.close === "function" && dialog.open) dialog.close();
  else dialog.removeAttribute("open");
  trigger?.setAttribute("aria-expanded", "false");
}

function rangeLabelKey(preset) {
  return {
    today: "filters.rangeToday",
    "1h": "filters.range1h",
    "24h": "filters.range24h",
    "7d": "filters.range7d",
    "14d": "filters.range14d",
    "30d": "filters.range30d",
    custom: "filters.custom",
  }[preset] || "filters.range24h";
}

function updateRangePickerTrigger() {
  elements.rangePickerLabel.textContent = t(rangeLabelKey(state.rangePreset));
  for (const button of elements.rangePresetOptions.querySelectorAll("[data-range-preset]")) {
    const active = button.dataset.rangePreset === state.rangePreset;
    button.classList.toggle("active", active);
    button.setAttribute("aria-pressed", String(active));
  }
}

function syncRangeInputs() {
  const range = state.rangeDraft || defaultCustomRange();
  elements.customFromDate.value = dateInputValue(range.from);
  elements.customFromTime.value = timeInputValue(range.from);
  elements.customToDate.value = dateInputValue(range.to);
  elements.customToTime.value = timeInputValue(range.to);
  state.rangeCalendarMonth = new Date(new Date(range.from * 1000).getFullYear(), new Date(range.from * 1000).getMonth(), 1);
  renderCalendar();
}

function readRangeInputs() {
  const from = parseDateTimeParts(elements.customFromDate.value, elements.customFromTime.value);
  const to = parseDateTimeParts(elements.customToDate.value, elements.customToTime.value);
  return { from, to };
}

function renderCalendar() {
  const month = state.rangeCalendarMonth;
  elements.calendarMonthLabel.textContent = new Intl.DateTimeFormat(locale, { year: "numeric", month: "long" }).format(month);
  elements.calendarWeekdays.replaceChildren();
  const weekdayFormatter = new Intl.DateTimeFormat(locale, { weekday: "short" });
  for (let index = 0; index < 7; index += 1) {
    const label = document.createElement("span");
    label.textContent = weekdayFormatter.format(new Date(2024, 0, index + 7));
    elements.calendarWeekdays.append(label);
  }
  elements.calendarDays.replaceChildren();
  const range = state.rangeDraft || defaultCustomRange();
  const start = new Date(range.from * 1000);
  const end = new Date(range.to * 1000);
  for (const day of calendarDays(month)) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "calendar-day";
    button.textContent = String(day.getDate());
    button.classList.toggle("outside-month", day.getMonth() !== month.getMonth());
    button.classList.toggle("today", sameLocalDay(day, new Date()));
    button.classList.toggle("in-range", day >= new Date(start.getFullYear(), start.getMonth(), start.getDate()) && day <= new Date(end.getFullYear(), end.getMonth(), end.getDate()));
    button.classList.toggle("endpoint", sameLocalDay(day, start) || sameLocalDay(day, end));
    button.addEventListener("click", () => {
      const draft = readRangeInputs();
      if (!Number.isFinite(draft.from) || !Number.isFinite(draft.to)) return;
      const timestamp = setDateKeepTime(state.activeRangeField === "start" ? draft.from : draft.to, day);
      state.rangeDraft = state.activeRangeField === "start"
        ? { from: timestamp, to: draft.to }
        : { from: draft.from, to: timestamp };
      syncRangeInputs();
      if (state.activeRangeField === "start") state.activeRangeField = "end";
    });
    elements.calendarDays.append(button);
  }
}

function openRangePicker() {
  if (dialogIsOpen(elements.rangePickerDialog)) {
    closeRangePicker();
    return;
  }
  state.rangeDraft = { ...(state.customRange || defaultCustomRange()) };
  elements.customRangeError.hidden = true;
  syncRangeInputs();
  openDialog(elements.rangePickerDialog, elements.rangePickerTrigger);
}

function closeRangePicker(discard = true) {
  if (discard) state.rangeDraft = null;
  closeDialog(elements.rangePickerDialog, elements.rangePickerTrigger);
}

function setRangePreset(preset) {
  if (preset === "custom") {
    state.rangePreset = "custom";
    state.rangeDraft = { ...(state.customRange || defaultCustomRange()) };
    updateRangePickerTrigger();
    syncRangeInputs();
    return;
  }
  state.rangePreset = preset;
  state.rangeDraft = null;
  updateRangePickerTrigger();
  closeRangePicker();
  refreshAll({ reloadFilters: true });
}

function updateBucketPicker() {
  elements.trendBucketLabel.textContent = state.trendBucket === "auto"
    ? t("trend.bucketAuto")
    : bucketDisplayLabel(state.trendBucket);
  for (const button of elements.bucketPresetOptions.querySelectorAll("[data-bucket]")) {
    const active = button.dataset.bucket === state.trendBucket;
    button.classList.toggle("active", active);
    button.setAttribute("aria-pressed", String(active));
  }
  const custom = splitBucketValue(state.trendBucket);
  if (custom) {
    elements.customBucketAmount.value = String(custom.amount);
    elements.customBucketUnit.value = custom.unit;
  }
}

function openBucketPicker() {
  if (dialogIsOpen(elements.bucketPickerDialog)) {
    closeBucketPicker();
    return;
  }
  elements.customBucketError.hidden = true;
  updateBucketPicker();
  openDialog(elements.bucketPickerDialog, elements.trendBucketTrigger);
}

function closeBucketPicker() {
  closeDialog(elements.bucketPickerDialog, elements.trendBucketTrigger);
}

function setTrendBucket(bucket) {
  state.trendBucket = bucket;
  updateBucketPicker();
  closeBucketPicker();
  refreshAll();
}

function updateFilterPlaceholders() {
  const labels = [
    [elements.nodeFilter, "filters.allNodes"],
    [elements.appFilter, "filters.allApps"],
    [elements.providerFilter, "filters.allProviders"],
    [elements.modelFilter, "filters.allModels"],
    [elements.sourceFilter, "filters.allSources"],
  ];
  for (const [select, key] of labels) {
    if (select.options[0]) select.options[0].textContent = t(key);
  }
}

function applyTheme() {
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
  elements.themeToggleLabel.textContent = t(
    theme === "dark" ? "theme.switchLightShort" : "theme.switchDarkShort",
  );
  elements.themeToggle.setAttribute(
    "aria-label",
    t(theme === "dark" ? "theme.switchLight" : "theme.switchDark"),
  );
  elements.themeToggle.setAttribute("aria-pressed", String(theme === "light"));
}

function applyTranslations() {
  document.documentElement.lang = locale;
  document.title = t("page.title");
  for (const element of document.querySelectorAll("[data-i18n]")) {
    element.textContent = t(element.dataset.i18n);
  }
  for (const element of document.querySelectorAll("[data-i18n-aria-label]")) {
    element.setAttribute("aria-label", t(element.dataset.i18nAriaLabel));
  }
  elements.languageToggle.textContent = t("language.switchShort");
  elements.languageToggle.setAttribute("aria-label", t("language.switch"));
  applyTheme();
  updateFilterPlaceholders();
  updateRangePickerTrigger();
  updateBucketPicker();
  renderCalendar();
  setConnection(state.connection.status, state.connection.key);
  elements.updatedAt.textContent = state.updatedAt
    ? t("status.updatedAt", { time: formatters.dateTime.format(state.updatedAt) })
    : t("status.neverUpdated");
  if (state.overview) renderOverview(state.overview);
  if (state.daily) renderDaily();
  renderEventRows(state.events, false);
  if (state.lastError) showError(state.lastError);
}

elements.themeToggle.addEventListener("click", () => {
  theme = theme === "dark" ? "light" : "dark";
  try {
    localStorage.setItem(themeStorageKey, theme);
  } catch {
    // The theme still changes for this page when persistent storage is unavailable.
  }
  applyTheme();
});

elements.languageToggle.addEventListener("click", () => {
  locale = locale === "zh-CN" ? "en-US" : "zh-CN";
  try {
    localStorage.setItem(localeStorageKey, locale);
  } catch {
    // The language still changes for this page when persistent storage is unavailable.
  }
  formatters = createFormatters(locale);
  applyTranslations();
});

elements.refreshButton.addEventListener("click", () => refreshAll({ reloadFilters: true }));
elements.rangePickerTrigger.addEventListener("click", openRangePicker);
elements.rangePresetOptions.addEventListener("click", (event) => {
  const button = event.target.closest("[data-range-preset]");
  if (button) setRangePreset(button.dataset.rangePreset);
});
elements.rangePickerForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const range = readRangeInputs();
  if (!Number.isFinite(range.from) || !Number.isFinite(range.to) || range.from >= range.to) {
    const error = new Error(t("error.invalidRange"));
    error.translationKey = "error.invalidRange";
    elements.customRangeError.textContent = t(error.translationKey);
    elements.customRangeError.hidden = false;
    return;
  }
  state.customRange = range;
  state.rangePreset = "custom";
  state.rangeDraft = null;
  updateRangePickerTrigger();
  closeRangePicker(false);
  refreshAll({ reloadFilters: true });
});
elements.closeRangePicker.addEventListener("click", () => closeRangePicker());
elements.cancelRange.addEventListener("click", () => closeRangePicker());
elements.rangePickerDialog.addEventListener("cancel", (event) => {
  event.preventDefault();
  closeRangePicker();
});
elements.previousCalendarMonth.addEventListener("click", () => {
  state.rangeCalendarMonth = new Date(state.rangeCalendarMonth.getFullYear(), state.rangeCalendarMonth.getMonth() - 1, 1);
  renderCalendar();
});
elements.nextCalendarMonth.addEventListener("click", () => {
  state.rangeCalendarMonth = new Date(state.rangeCalendarMonth.getFullYear(), state.rangeCalendarMonth.getMonth() + 1, 1);
  renderCalendar();
});
for (const input of [elements.customFromDate, elements.customFromTime, elements.customToDate, elements.customToTime]) {
  input.addEventListener("change", () => {
    const range = readRangeInputs();
    if (Number.isFinite(range.from) && Number.isFinite(range.to)) {
      state.rangeDraft = range;
      renderCalendar();
    }
  });
}
for (const field of elements.rangePickerForm.querySelectorAll("[data-range-field]")) {
  field.addEventListener("click", () => {
    state.activeRangeField = field.dataset.rangeField;
  });
}
elements.trendBucketTrigger.addEventListener("click", openBucketPicker);
elements.bucketPresetOptions.addEventListener("click", (event) => {
  const button = event.target.closest("[data-bucket]");
  if (button) setTrendBucket(button.dataset.bucket);
});
elements.closeBucketPicker.addEventListener("click", closeBucketPicker);
elements.bucketPickerDialog.addEventListener("cancel", (event) => {
  event.preventDefault();
  closeBucketPicker();
});
document.addEventListener("pointerdown", (event) => {
  const pickers = [
    [elements.rangePickerDialog, elements.rangePickerTrigger, closeRangePicker],
    [elements.bucketPickerDialog, elements.trendBucketTrigger, closeBucketPicker],
  ];
  for (const [dialog, trigger, close] of pickers) {
    if (!dialogIsOpen(dialog) || dialog.contains(event.target) || trigger.contains(event.target)) continue;
    close();
  }
});
window.addEventListener("resize", positionOpenPickers);
window.addEventListener("scroll", positionOpenPickers, true);
elements.applyCustomBucket.addEventListener("click", () => {
  const bucket = parseBucketValue(elements.customBucketAmount.value, elements.customBucketUnit.value);
  if (!bucket) {
    elements.customBucketError.textContent = t("error.invalidBucket");
    elements.customBucketError.hidden = false;
    return;
  }
  setTrendBucket(bucket);
});
elements.bucketPickerForm.addEventListener("submit", (event) => event.preventDefault());

elements.rangePickerForm.addEventListener("invalid", () => {
  elements.customRangeError.textContent = t("error.invalidRange");
  elements.customRangeError.hidden = false;
}, true);

for (const select of [elements.nodeFilter, elements.appFilter, elements.providerFilter, elements.modelFilter, elements.sourceFilter]) {
  select.addEventListener("change", () => refreshAll());
}

elements.trendMetric.addEventListener("change", renderTrend);
elements.dailyMetric.addEventListener("change", renderDaily);
elements.breakdownTabs.addEventListener("click", (event) => {
  const button = event.target.closest("[data-dimension]");
  if (!button) return;
  state.breakdownDimension = button.dataset.dimension;
  for (const tab of elements.breakdownTabs.querySelectorAll(".tab")) {
    tab.classList.toggle("active", tab === button);
  }
  renderBreakdown();
});
elements.loadMore.addEventListener("click", () => loadEvents({ append: true }).catch(showError));

document.addEventListener("visibilitychange", () => {
  if (!document.hidden) refreshAll();
});

updateRangePickerTrigger();
updateBucketPicker();
applyTheme();
applyTranslations();
if (typeof ResizeObserver === "function") {
  new ResizeObserver(syncBrandMarkSize).observe(elements.brandLockup);
}
window.addEventListener("resize", syncBrandMarkSize);
syncBrandMarkSize();
refreshAll({ reloadFilters: true });
setInterval(() => {
  if (!document.hidden) refreshAll();
}, 30_000);
