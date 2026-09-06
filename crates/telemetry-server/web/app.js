import * as echarts from "./vendor/echarts.esm.min.mjs";
import {
  buildDailyOption,
  buildQuotaOption,
  buildTrendOption,
  tooltipMarkup,
  usageTooltipMarkup,
  dailyCalendarLayout,
} from "./charts.js";
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
import {
  filterQuotaProviders,
  optionalQuotaNumber,
  quotaAmount,
  quotaAmountRange,
  quotaMetricIdentity,
  quotaPercentage,
  splitQuotaPoints,
  quotaSnapshotRows,
  createQuotaSnapshotLoader,
} from "./quota-view.js";

import { providerIdentity, metricIdentity, providerName, providerSelected, metricSelected, mergeProviders, renderPicker } from "./quota-settings.js";

const $ = (id) => document.getElementById(id);
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
  coverageText: $("coverageText"),
  quotaNodeFilter: $("quotaNodeFilter"),
  quotaProviderFilter: $("quotaProviderFilter"),
  quotaMetricFilter: $("quotaMetricFilter"),
  quotaCards: $("quotaCards"),
  quotaEmpty: $("quotaEmpty"),
  quotaChart: $("quotaChart"),
  quotaBucket: $("quotaBucket"),
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
  dailyHeatmap: $("dailyHeatmap"),
};

const state = {
  overview: null,
  daily: null,
  quota: null,
  settings: null,
  quotaSelection: undefined,
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
  quotaGranularity: "auto",
  bucketTarget: "trend",
  resources: {},
  resourceErrors: new Map(),
  quotaPlots: [],
  connection: { status: "", key: "status.connecting" },
};

const chartInstances = {
  trend: null,
  quota: null,
  daily: null,
};

const quotaSnapshots = createQuotaSnapshotLoader((at, signal) => fetchJson(`/v3/dashboard/quota/at?at=${at}`, signal));

function reducedMotion() {
  return typeof window.matchMedia === "function"
    && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function cssColor(name, fallback) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

function chartPalette() {
  return {
    text: cssColor("--text", "#f3f7fb"),
    muted: cssColor("--muted", "#94a2ba"),
    faint: cssColor("--faint", "#63708a"),
    border: cssColor("--border", "rgba(148, 163, 184, 0.16)"),
    borderStrong: cssColor("--border-strong", "rgba(148, 163, 184, 0.3)"),
    surfaceSolid: cssColor("--surface-solid", "#111827"),
    surfaceRaised: cssColor("--surface-raised", "#182237"),
    accent: cssColor("--accent", "#62e6d1"),
    accentArea: cssColor("--accent-soft", "rgba(45, 212, 191, 0.12)"),
    accentShadow: "rgba(45, 212, 191, 0.28)",
    transparent: "rgba(0, 0, 0, 0)",
  };
}

function ensureChart(name, element) {
  if (!chartInstances[name]) {
    chartInstances[name] = echarts.init(element, null, { renderer: "canvas" });
    if (name === "quota") {
      chartInstances[name].on("hideTip", quotaSnapshots.cancel);
      chartInstances[name].on("legendselectchanged", () => { quotaSnapshots.cancel(); chartInstances[name].dispatchAction({ type: "hideTip" }); });
      chartInstances[name].getZr().on("globalout", quotaSnapshots.cancel);
    }
  }
  return chartInstances[name];
}

function updateChart(name, element, option) {
  element.hidden = false;
  const chart = ensureChart(name, element);
  chart.resize();
  chart.setOption(option, { replaceMerge: ["series"], lazyUpdate: false });
}

function clearChart(name, element) {
  chartInstances[name]?.clear();
  element.hidden = true;
}

function resizeCharts() {
  for (const chart of Object.values(chartInstances)) chart?.resize();
  if (state.daily) renderDaily();
  if (state.overview) renderTrend();
  if (state.quota) renderQuotaChart(quotaProviders());
}

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
  const filters = await fetchJson(`/v3/dashboard/filters?${params}`, signal);
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
  animateMetric($("kpiRequests"), summary.totalRequests, formatTokens);
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
  elements.trendEmpty.hidden = points.length > 0;
  if (state.overview?.range?.bucket) {
    elements.resolvedBucket.textContent = t("trend.resolvedBucket", {
      bucket: state.overview.range.bucket,
    });
  }
  if (!points.length) {
    clearChart("trend", elements.trendChart);
    return;
  }

  const metric = elements.trendMetric.value;
  const range = state.overview.range;
  const spanSeconds = Math.max(1, Number(range.to) - Number(range.from));
  const palette = chartPalette();
  updateChart("trend", elements.trendChart, buildTrendOption({
    points,
    metric,
    range,
    chartWidth: elements.trendChart.clientWidth,
    palette,
    formatAxis: (timestampMs) => formatTrendAxis(timestampMs / 1_000, spanSeconds),
    formatValue: (value) => trendValueLabel(value, metric),
    formatTooltip: (point) => usageTooltip(
      point,
      formatters.dateTime.format(point.bucketStart * 1_000),
    ),
    ariaDescription: t("trend.chartAria"),
    reducedMotion: reducedMotion(),
  }));
}

function usageTooltip(point, titleText) {
  const lines = [
    ["", t("trend.tooltipInput", { value: formatTokens(point.inputTokens) })],
    ["", t("trend.tooltipFreshInput", { value: formatTokens(point.freshInputTokens) })],
    ["", t("trend.tooltipCacheCreation", { value: formatTokens(point.cacheCreationTokens) })],
    ["", t("trend.tooltipCacheRead", { value: formatTokens(point.cacheReadTokens) })],
    ["", t("trend.tooltipOutput", { value: formatTokens(point.outputTokens) })],
    ["", t("trend.tooltipRequests", { value: formatters.integerNumber.format(point.totalRequests) })],
    ["", t("trend.tooltipSuccess", { value: formatPercent(point.successRate) })],
    ["", t("trend.tooltipCost", { value: formatters.moneyNumber.format(point.totalCostUsd) })],
    ["", t("trend.tooltipLatency", { value: formatLatency(point.avgLatencyMs) })],
  ];
  return usageTooltipMarkup(point, titleText, lines, (value) => formatters.integerNumber.format(value));
}

function quotaTooltip(params, ticket, callback) {
  const items = Array.isArray(params) ? params : [params];
  const raw = items.find((item) => item?.axisValue != null || item?.value?.[0] != null);
  if (!raw) return "";
  const at = Math.floor(Number(raw.axisValue ?? raw.value[0]) / 1000);
  if (!Number.isFinite(at)) return "";
  const title = formatters.quotaDateTime.format(at * 1000);
  const legend = chartInstances.quota?.getOption().legend?.[0]?.selected || {};
  const plots = state.quotaPlots;
  const markup = (snapshot, placeholder = null) => tooltipMarkup(title,
    quotaSnapshotRows(plots, snapshot || { at, metrics: [] }, legend).map(({ plot, point, value }) => [
      plot.name,
      placeholder || (value == null ? t("quota.noData") :
        (plot.axis === "amount" ? formatTokens(value) + (plot.unit ? " " + plot.unit : "") : Number(value).toFixed(1) + "%")
        + " · " + formatters.quotaDateTime.format(point.sampledAt * 1000)),
    ]));
  const cached = quotaSnapshots.peek(at);
  if (cached) { quotaSnapshots.cancel(); return markup(cached); }
  quotaSnapshots.request(at, (snapshot) => callback?.(ticket, markup(snapshot)),
    () => callback?.(ticket, markup(null, t("quota.loadFailed"))));
  return markup(null, t("quota.loading"));
}

function dailyTooltip(params) {
  const point = params?.data?.source;
  if (!point) return "";
  const date = new Date(`${params.data.value[0]}T00:00:00`);
  return usageTooltip(
    point,
    new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(date),
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
  if (coverage.firstEventAt == null || coverage.lastEventAt == null) {
    elements.coverageText.textContent = t("coverage.empty");
    return;
  }
  const scope = coverage.includesDetail && coverage.includesRollups
    ? t("coverage.detailAndRollup")
    : coverage.includesRollups
      ? t("coverage.rollupOnly")
      : t("coverage.detailOnly");
  elements.coverageText.textContent = t("coverage.range", {
    from: formatters.dateTime.format(coverage.firstEventAt * 1000),
    to: formatters.dateTime.format(coverage.lastEventAt * 1000),
    scope,
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

function quotaValueLabel(metric) {
  const percentage = quotaPercentage(metric);
  if (percentage != null) return formatPercent(percentage);
  const unit = metric.unit ? ` ${metric.unit}` : "";
  const number = (value) => formatters.compactNumber.format(Number(value));
  if (optionalQuotaNumber(metric.remaining) != null && optionalQuotaNumber(metric.total) != null) {
    return `${number(metric.remaining)} / ${number(metric.total)}${unit}`;
  }
  if (optionalQuotaNumber(metric.remaining) != null) return `${number(metric.remaining)}${unit}`;
  if (optionalQuotaNumber(metric.used) != null) return `${number(metric.used)}${unit} ${t("quota.used")}`;
  if (optionalQuotaNumber(metric.total) != null) return `${number(metric.total)}${unit}`;
  return "—";
}

function quotaDisplayName(provider) { return providerName(provider, state.settings); }

function quotaProviders() {
  return (state.quota?.providers || []).filter((provider) =>
    (!elements.quotaNodeFilter.value || provider.nodeId === elements.quotaNodeFilter.value)
    && providerSelected(state.quotaSelection, provider));
}

function quotaCatalog() {
  return mergeProviders(state.quota?.providers || [], state.settings, state.quotaSelection);
}

function updateQuotaFilters() {
  const providers = quotaCatalog();
  const nodes = new Map(providers.map((p) => [p.nodeId, p.nodeName || p.nodeId]));
  setSelectOptions(elements.quotaNodeFilter,
    [...nodes].map(([value, label]) => ({ value, label })), t("quota.allNodes"));
  const selected = state.quotaSelection;
  const pickerLabels = { allLabel: t("quota.selectAll"), noneLabel: t("quota.selectNone") };
  renderPicker(elements.quotaProviderFilter, {
    ...pickerLabels, title: t("quota.provider"),
    groups: [{ options: providers.map((p) => ({ value: providerIdentity(p), label: `${p.nodeName || p.nodeId} / ${providerName(p, state.settings)}${p.unavailable ? ' (' + t("quota.unavailable") + ')' : ''}` })) }],
    selected: selected == null ? null : selected.map(providerIdentity),
    onChange: (keys) => {
      state.quotaSelection = keys === null ? null : keys.map((key) => {
        const [nodeId, providerId] = JSON.parse(key);
        return selected?.find((p) => providerIdentity(p) === key) || { nodeId, providerId, metrics: null };
      });
      renderQuota();
    },
  });
  const chosen = providers.filter((p) => providerSelected(selected, p)
    && (!elements.quotaNodeFilter.value || p.nodeId === elements.quotaNodeFilter.value));
  const metricKey = (p, m) => JSON.stringify([p.nodeId, p.providerId, m.key, m.kind, m.unit || ""]);
  const groups = chosen.map((p) => ({
    label: `${p.nodeName || p.nodeId} / ${providerName(p, state.settings)}`,
    options: p.metrics.map((m) => ({ value: metricKey(p, m), label: `${m.label || m.key} · ${m.unit || m.kind}` })),
  }));
  const allMetrics = chosen.every((p) => selected == null || selected.find((item) => providerIdentity(item) === providerIdentity(p))?.metrics == null);
  renderPicker(elements.quotaMetricFilter, {
    ...pickerLabels, title: t("quota.metric"), groups,
    selected: allMetrics ? null : chosen.flatMap((p) => p.metrics.filter((m) => metricSelected(selected, p, m)).map((m) => metricKey(p, m))),
    onChange: (keys) => {
      const selectedKeys = new Set(keys || []);
      const entries = selected == null ? providers.map((p) => ({ nodeId: p.nodeId, providerId: p.providerId, metrics: null })) : structuredClone(selected);
      for (const entry of entries) {
        const p = chosen.find((p) => providerIdentity(p) === providerIdentity(entry));
        if (p) entry.metrics = keys === null ? null : p.metrics.filter((m) => selectedKeys.has(metricKey(p, m))).map(({ key, kind, unit }) => ({ key, kind, unit: unit || null }));
      }
      state.quotaSelection = entries;
      renderQuota();
    },
  });
}

function appendQuotaDetail(container, labelText, valueText) {
  const item = document.createElement("div");
  item.className = "quota-detail";
  const label = document.createElement("span");
  label.textContent = labelText;
  const value = document.createElement("strong");
  value.textContent = valueText;
  item.append(label, value);
  container.append(item);
}

function renderQuotaCards(providers) {
  elements.quotaCards.replaceChildren();

  for (const provider of providers) {
    const card = document.createElement("article");
    card.className = `quota-card quota-status-${provider.status || "unknown"}`;
    const heading = document.createElement("div");
    heading.className = "quota-card-heading";
    const title = document.createElement("div");
    const providerName = document.createElement("strong");
    providerName.textContent = quotaDisplayName(provider);
    const nodeName = document.createElement("small");
    nodeName.textContent = `${provider.nodeName || provider.nodeId} · ${provider.providerId}`;
    title.append(providerName, nodeName);
    const status = document.createElement("span");
    status.className = "quota-status";
    status.textContent = t(`quota.status.${provider.status || "unknown"}`);
    heading.append(title, status);
    card.append(heading);

    const metrics = (provider.current || []).filter(
      (metric) => metricSelected(state.quotaSelection, provider, metric),
    );
    const details = document.createElement("div");
    details.className = "quota-details";
    if (metrics.length) {
      for (const metric of metrics) {
        appendQuotaDetail(details, metric.label || metric.key, quotaValueLabel(metric));
        if (metric.resetsAt != null) {
          appendQuotaDetail(
            details,
            t("quota.reset"),
            formatters.quotaDateTime.format(metric.resetsAt * 1000),
          );
        }
      }
    } else {
      appendQuotaDetail(details, t("quota.current"), t("quota.noSuccessfulSample"));
    }
    appendQuotaDetail(
      details,
      t("quota.lastSuccess"),
      provider.lastSuccessAt == null
        ? "—"
        : formatters.quotaDateTime.format(provider.lastSuccessAt * 1000),
    );
    appendQuotaDetail(
      details,
      t("quota.lastCheck"),
      formatters.quotaDateTime.format(provider.checkedAt * 1000),
    );
    card.append(details);
    elements.quotaCards.append(card);
  }
}

function quotaSeries(providers) {

  return providers.flatMap((provider) => (provider.series || [])
    .filter((series) => metricSelected(state.quotaSelection, provider, series))
    .map((series) => ({ ...series, provider }))
    .filter((series) => series.points?.length));
}

function renderQuotaChart(providers) {
  const series = quotaSeries(providers);
  const plottedSeries = series.flatMap((item, seriesIndex) => {
    const plots = [];
    if (item.points.some((point) => quotaPercentage(point) != null)) {
      plots.push({ ...item, axis: "percent", seriesIndex, value: quotaPercentage });
    }
    if (item.points.some((point) => quotaAmount(point) != null)) {
      plots.push({ ...item, axis: "amount", seriesIndex, value: quotaAmount });
    }
    return plots;
  });
  if (!plottedSeries.length) {
    clearChart("quota", elements.quotaChart);
    return;
  }

  const amountValues = plottedSeries
    .filter((item) => item.axis === "amount")
    .flatMap((item) => item.points.map(item.value))
    .filter(Number.isFinite);
  const palette = chartPalette();
  const colors = [
    palette.accent,
    cssColor("--violet", "#a78bfa"),
    cssColor("--blue", "#60a5fa"),
    cssColor("--orange", "#fbbf24"),
    cssColor("--success", "#54e39a"),
    cssColor("--danger", "#fb7185"),
    "#38bdf8",
    "#f472b6",
  ];
  const range = state.quota.range;
  const spanSeconds = Math.max(1, Number(range.to) - Number(range.from));
  const plots = plottedSeries.map((item) => {
    const axisLabel = item.axis === "percent" ? "%" : item.unit || t("quota.amountAxis");
    return {
      id: JSON.stringify([
        item.provider.nodeId,
        item.provider.providerId,
        item.key,
        item.kind,
        item.unit,
        item.axis,
      ]),
      name: `${item.provider.nodeName || item.provider.nodeId} / ${quotaDisplayName(item.provider)} / ${item.label || item.key} · ${axisLabel}`,
      axis: item.axis,
      unit: item.unit,
      metricId: JSON.stringify([item.provider.nodeId, item.provider.providerId, item.key, item.kind, item.unit || ""]),
      color: colors[item.seriesIndex % colors.length],
      value: item.value,
      segments: splitQuotaPoints(
        item.points,
        state.quota.range.bucketSeconds,
        item.value,
      ),
    };
  });
  state.quotaPlots = plots;
  updateChart("quota", elements.quotaChart, buildQuotaOption({
    plots,
    range,
    amountRange: quotaAmountRange(amountValues),
    chartWidth: elements.quotaChart.clientWidth,
    palette,
    formatAxis: (timestampMs) => formatTrendAxis(timestampMs / 1_000, spanSeconds),
    formatAmount: (value) => formatters.compactNumber.format(value),
    formatTooltip: quotaTooltip,
    percentAxisName: t("quota.percentAxis"),
    amountAxisName: t("quota.amountAxis"),
    ariaDescription: t("quota.chartAria"),
    reducedMotion: reducedMotion(),
  }));
}

function renderQuota() {
  quotaSnapshots.cancel();
  chartInstances.quota?.dispatchAction({ type: "hideTip" });
  updateQuotaFilters();
  const providers = quotaProviders();
  elements.quotaEmpty.hidden = providers.length > 0;
  renderQuotaCards(providers);
  renderQuotaChart(providers);
  elements.quotaBucket.textContent = state.quota?.range
    ? t("quota.resolvedBucket", { bucket: state.quota.range.bucket })
    : "";
}

function dailyMetricValue(point, metric) {
  return Number(point?.[metric] || 0);
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
  elements.dailyEmpty.hidden = points.length > 0;
  if (!points.length) {
    clearChart("daily", elements.dailyHeatmap);
    return;
  }

  const first = new Date(points[0].bucketStart * 1000);
  const last = new Date(points.at(-1).bucketStart * 1000);
  first.setHours(0, 0, 0, 0);
  last.setHours(0, 0, 0, 0);

  const values = points.map((point) => dailyMetricValue(point, metric)).filter((value) => value > 0).sort((a, b) => a - b);
  const maxValue = values.at(-1) || 0;
  const levelFor = (value) => {
    if (value <= 0 || maxValue <= 0) return 0;
    if (value === maxValue) return 4;
    const rank = values.findIndex((item) => item >= value);
    return Math.max(1, Math.min(4, Math.ceil((rank + 1) / values.length * 4)));
  };
  const weekdayFormatter = new Intl.DateTimeFormat(locale, { weekday: "short" });
  const monthFormatter = new Intl.DateTimeFormat(locale, { month: "short" });
  const chartPoints = points.map((point) => {
    const value = dailyMetricValue(point, metric);
    return {
      date: localDateKey(new Date(point.bucketStart * 1_000)),
      value,
      level: levelFor(value),
      source: point,
    };
  });
  const palette = chartPalette();
  const dailyRange = [localDateKey(first), localDateKey(last)];
  elements.dailyHeatmap.style.height = `${dailyCalendarLayout(dailyRange, elements.dailyHeatmap.clientWidth).height}px`;
  updateChart("daily", elements.dailyHeatmap, buildDailyOption({
    points: chartPoints,
    range: [localDateKey(first), localDateKey(last)],
    chartWidth: elements.dailyHeatmap.clientWidth,
    palette,
    colors: [
      palette.surfaceRaised,
      "rgba(45, 212, 191, 0.24)",
      "rgba(45, 212, 191, 0.45)",
      "rgba(45, 212, 191, 0.68)",
      cssColor("--accent-strong", "#2dd4bf"),
    ],
    dayNames: Array.from({ length: 7 }, (_, weekday) => (
      weekdayFormatter.format(new Date(2024, 0, weekday))
    )),
    monthNames: Array.from({ length: 12 }, (_, month) => (
      monthFormatter.format(new Date(2024, month, 1))
    )),
    formatTooltip: dailyTooltip,
    ariaDescription: t("daily.chartAria"),
    reducedMotion: reducedMotion(),
  }));
}

function eventStatus(statusCode) {
  if (statusCode >= 200 && statusCode < 300) return [t("events.success"), "status-pill"];
  return [String(statusCode || t("events.failure")), "status-pill failed"];
}

function renderEventRows(items, append) {
  if (!append) elements.eventRows.replaceChildren();
  for (const item of items) {
    const row = document.createElement("tr");
    row.title = `${t("events.requestId", { id: item.requestId })} · UUID: ${item.nodeId || "—"}`;
    appendCell(row, formatters.dateTime.format(item.createdAt * 1000));
    appendCell(row, item.nodeName || item.nodeId || "—");
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
    const response = await fetchJson(`/v3/dashboard/events?${params}`, signal);
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

async function resource(name, fetcher, commit, parent) {
  state.resources[name]?.abort();
  const controller = new AbortController();
  state.resources[name] = controller;
  const abort = () => controller.abort();
  if (parent?.aborted) abort();
  parent?.addEventListener("abort", abort, { once: true });
  try {
    const result = await fetcher(controller.signal);
    if (controller.signal.aborted || state.resources[name] !== controller) return;
    commit(result);
    state.resourceErrors.delete(name);
  } catch (error) {
    if (error.name !== "AbortError" && !controller.signal.aborted) {
      state.resourceErrors.set(name, error);
      showError(error);
    }
    throw error;
  } finally {
    parent?.removeEventListener("abort", abort);
    if (state.resources[name] === controller) delete state.resources[name];
  }
}

function refreshOverview(parent) {
  const params = baseParams(true);
  params.set("bucket", state.trendBucket);
  return resource("overview", (signal) => fetchJson(`/v3/dashboard/overview?${params}`, signal), renderOverview, parent);
}

function refreshQuota(parent) {
  const params = baseParams(false);
  params.set("bucket", state.quotaGranularity);
  return resource("quota", async (signal) => {
    const response = await fetchJson(`/v3/dashboard/quota?${params}`, signal);
    if (state.quotaSelection === undefined) await state.settingsPromise;
    return response;
  }, (response) => {
    state.quota = response;
    quotaSnapshots.invalidate();
    renderQuota();
  }, parent);
}

async function refreshAll({ reloadFilters = false } = {}) {
  state.requestController?.abort();
  const controller = new AbortController();
  state.requestController = controller;
  // Invalidate independently started requests when the shared range changes.
  for (const pending of Object.values(state.resources)) pending.abort();
  elements.refreshButton.disabled = true;
  setConnection("", "status.refreshing");
  try {
    if (reloadFilters) await refreshFilters(controller.signal);
    if (controller.signal.aborted) return;
    state.settingsPromise = resource("settings", (signal) => fetchJson("/v3/dashboard/settings", signal), (settings) => {
      const changed = JSON.stringify(state.settings) !== JSON.stringify(settings);
      state.settings = settings;
      if (state.quotaSelection === undefined) state.quotaSelection = structuredClone(settings.quotaDefaults.providers);
      if (changed && state.quota) renderQuota();
    }, controller.signal);
    await Promise.allSettled([
      state.settingsPromise,
      refreshOverview(controller.signal),
      resource("daily", (signal) => fetchJson(`/v3/dashboard/daily?${dailyParams(true)}`, signal), (daily) => {
        state.daily = daily; renderDaily();
      }, controller.signal),
      refreshQuota(controller.signal),
      resource("events", (signal) => loadEvents({ append: false, signal }), () => {}, controller.signal),
    ]);
    if (controller.signal.aborted) return;
    if (state.resourceErrors.size) throw state.resourceErrors.values().next().value;
    clearError();
    setConnection("online", "status.online");
    state.updatedAt = Date.now();
    elements.updatedAt.textContent = t("status.updatedAt", { time: formatters.dateTime.format(state.updatedAt) });
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
  positionDialog(elements.bucketPickerDialog, bucketTrigger());
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
    "1y": "filters.range1y",
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

function bucketTrigger() { return state.bucketTarget === "quota" ? $("quotaBucketTrigger") : elements.trendBucketTrigger; }
function updateBucketPicker() {
  elements.trendBucketLabel.textContent = state.trendBucket === "auto" ? t("trend.bucketAuto") : bucketDisplayLabel(state.trendBucket);
  $("quotaGranularityLabel").textContent = state.quotaGranularity === "auto" ? t("trend.bucketAuto") : bucketDisplayLabel(state.quotaGranularity);
  const bucket = state.bucketTarget === "quota" ? state.quotaGranularity : state.trendBucket;
  for (const button of elements.bucketPresetOptions.querySelectorAll("[data-bucket]")) {
    const active = button.dataset.bucket === bucket;
    button.classList.toggle("active", active);
    button.setAttribute("aria-pressed", String(active));
  }
  const custom = splitBucketValue(bucket);
  if (custom) { elements.customBucketAmount.value = String(custom.amount); elements.customBucketUnit.value = custom.unit; }
}
function openBucketPicker(target = "trend") {
  if (dialogIsOpen(elements.bucketPickerDialog)) {
    const same = state.bucketTarget === target;
    closeBucketPicker();
    if (same) return;
  }
  state.bucketTarget = target;
  elements.customBucketError.hidden = true;
  updateBucketPicker();
  openDialog(elements.bucketPickerDialog, bucketTrigger());
}
function closeBucketPicker() { closeDialog(elements.bucketPickerDialog, bucketTrigger()); }
function setTrendBucket(bucket) {
  const quota = state.bucketTarget === "quota";
  if (quota) state.quotaGranularity = bucket; else state.trendBucket = bucket;
  updateBucketPicker(); closeBucketPicker();
  (quota ? refreshQuota() : refreshOverview()).catch((error) => { if (error.name !== "AbortError") showError(error); });
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
  if (state.quota) renderQuota();
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
  if (state.overview) renderTrend();
  if (state.quota) renderQuota();
  if (state.daily) renderDaily();
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
elements.trendBucketTrigger.addEventListener("click", () => openBucketPicker("trend"));
$("quotaBucketTrigger").addEventListener("click", () => openBucketPicker("quota"));
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
    [elements.bucketPickerDialog, bucketTrigger(), closeBucketPicker],
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
$("restoreQuotaDefaults").addEventListener("click", () => {
  state.quotaSelection = structuredClone(state.settings?.quotaDefaults.providers ?? null);
  elements.quotaNodeFilter.value = "";
  renderQuota();
});
for (const select of [elements.quotaNodeFilter]) {
  select.addEventListener("change", renderQuota);
}
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
  const chartResizeObserver = new ResizeObserver(() => {
    requestAnimationFrame(resizeCharts);
  });
  for (const element of [elements.trendChart, elements.quotaChart, elements.dailyHeatmap]) {
    chartResizeObserver.observe(element);
  }
}
window.addEventListener("resize", () => {
  syncBrandMarkSize();
  resizeCharts();
});
syncBrandMarkSize();
refreshAll({ reloadFilters: true });
setInterval(() => {
  if (!document.hidden) refreshAll();
}, 30_000);
