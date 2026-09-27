import { createRequestPool } from "./requests.js";
import * as echarts from "./vendor/echarts.esm.min.mjs";
import {
  buildDailyOption,
  buildQuotaOption,
  buildTrendOption,
  buildComparisonOption,
  accumulateTrendPoints,
  tooltipMarkup,
  usageTooltipMarkup,
  dailyCalendarLayout,
} from "./charts.js";
import { createHistoryQuotaService, createHistoryQuotaLoader, historyIdentity } from "./history-quota.js";
import { createEstimatedQuotaLoader } from "./estimated-quota.js";
import { createPastResetLoader } from "./past-resets.js";
import { resetCycleGroups, resolveResetCycleChoice } from "./reset-cycle.js";
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
  RANGE_PRESETS,
  rangeIncludesNow,
  resolveAllTimeRange,
  resolvePresetRange,
  sameLocalDay,
  selectCalendarRange,
  splitBucketValue,
  startOfLocalDayMs,
  timeInputPlaceholder,
  timeInputValue,
} from "./range.js";
import {
  filterQuotaProviders,
  estimatedQuota,
  optionalQuotaNumber,
  quotaAmount,
  quotaAmountRange,
  quotaExhaustion,
  quotaMetricIdentity,
  quotaPercentage,
  quotaPrediction,
  quotaPredictionRows,
  quotaPredictionLookback,
  quotaPredictionRange,
  quotaResetTiers,
  splitQuotaPoints,
  quotaSnapshotRows,
  createQuotaSnapshotLoader,
} from "./quota-view.js";

import { providerIdentity, metricIdentity, providerName, providerSelected, metricSelected, mergeProviders, renderPicker } from "./quota-settings.js";

const fetchJson = createRequestPool(async (url, signal) => {
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
});

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
  rangePickerTrigger: $("rangePickerTrigger"),
  rangePickerLabel: $("rangePickerLabel"),
  rangePickerDialog: $("rangePickerDialog"),
  rangePickerForm: $("rangePickerForm"),
  rangePresetOptions: $("rangePresetOptions"),
  customRangeEditor: $("customRangeEditor"),
  customRangeError: $("customRangeError"),
  lastResetEditor: $("lastResetEditor"),
  lastResetProvider: $("lastResetProvider"),
  lastResetTier: $("lastResetTier"),
  lastResetFrom: $("lastResetFrom"),
  lastResetTo: $("lastResetTo"),
  lastResetError: $("lastResetError"),
  resetCycleSelect: $("resetCycleSelect"),
  resetCycleCurrent: $("resetCycleCurrent"),
  resetCycleLast: $("resetCycleLast"),
  resetCycleRetry: $("resetCycleRetry"),
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
  trendCumulativeToggle: $("trendCumulativeToggle"),
  trendCumulative: $("trendCumulative"),
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
  kpiCostAdjustment: $("kpiCostAdjustment"),
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
  quotaPredictions: new Set(),
  quotaPredictionHistory: new Map(),
  breakdownDimension: "nodes",
  eventCursor: null,
  requestController: null,
  eventsLoading: false,
  eventsGeneration: 0,
  events: [],
  lastError: null,
  updatedAt: null,
  rangePreset: "24h",
  timeFormat: "24h",
  modelBillingMultipliers: [],
  customRange: null,
  rangeDraft: null,
  lastResetSelection: null,
  rangePresetDraft: null,
  rangePresetController: null,
  firstRecordedAt: null,
  lastResetDraft: null,
  resetCycleHistory: { loading: false, error: false, response: null },
  rangeCalendarMonth: new Date(new Date().getFullYear(), new Date().getMonth(), 1),
  activeRangeField: "start",
  comparison: { enabled: false, selection: null, savedMetric: null, predictDefaultPending: null, estimatedQuota: false, historyEnabled: true, historyConfig: { mode: "full" } },
  trendCumulative: false,
  chartBucket: "auto",
  bucketTarget: "trend",
  resources: {},
  resourceErrors: new Map(),
  quotaPlots: [],
  connection: { status: "", key: "status.connecting" },
};

const historyQuotaService = createHistoryQuotaService(fetchJson, async (url, body, signal) => {
  const response = await fetch(url, { method: "POST", signal,
    headers: { "Content-Type": "application/json", Accept: "application/json" }, body: JSON.stringify(body) });
  if (!response.ok) throw new Error(`Quota reference: ${response.status}`);
  return response.json();
});
const historyQuotaLoader = createHistoryQuotaLoader(historyQuotaService, () => renderTrend());
const historyPreviewLoader = createHistoryQuotaLoader(historyQuotaService, () => renderHistoryDialog());
let historyDraft = null;
const estimatedQuotaLoader = createEstimatedQuotaLoader(fetchJson, () => renderTrend());
const predictionChartCache = new Map();
const resolvedPredictionCache = new Map();

const chartInstances = {
  trend: null,
  quota: null,
  daily: null,
};

const resetCycleLoader = createPastResetLoader(
  (signal) => fetchResetCycleContext(signal),
  (history) => {
    state.resetCycleHistory = { ...history, response: history.response ?? state.resetCycleHistory.response };
    if (pickerPreset() === "last-reset") renderLastResetEditor();
  },
);

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

const pendingChartOptions = new Map();
let chartFrame = null;

function updateChart(name, element, option) {
  element.hidden = false;
  const chart = ensureChart(name, element);
  const identity = name === "trend" ? JSON.stringify([state.comparison.enabled,
    state.comparison.selection, elements.trendMetric.value, state.trendCumulative, state.chartBucket])
    : JSON.stringify(option.series?.map((series) => series.id));
  for (const series of option.series || []) series.animation = Boolean(option.animation);
  if (element.dataset.chartIdentity === identity) {
    option.animation = false;
    for (const series of option.series || []) { series.animation = false; series.universalTransition = false; }
  }
  element.dataset.chartIdentity = identity;
  pendingChartOptions.set(name, { chart, option });
  if (!chartFrame) chartFrame = requestAnimationFrame(() => {
    chartFrame = null;
    const updates = [...pendingChartOptions.values()];
    pendingChartOptions.clear();
    for (const { chart, option } of updates) {
      chart.resize();
      chart.setOption(option, { replaceMerge: ["series", "yAxis"], lazyUpdate: false });
    }
  });
}

function clearChart(name, element) {
  pendingChartOptions.delete(name);
  chartInstances[name]?.clear();
  element.hidden = true;
}

function resizeCharts() {
  for (const chart of Object.values(chartInstances)) chart?.resize();
  if (state.daily) renderDaily();
  if (state.overview) renderTrend();
  if (state.quota) renderQuotaChart(quotaProviders());
}

function resetGroups(nowMs = Date.now()) {
  return resetCycleGroups(state.resetCycleHistory.response, state.resetCycleHistory.response?.currentProviders || state.quota?.providers || [], nowMs);
}

function resolveLastResetChoice(nowMs = Date.now(), selection = state.lastResetSelection) {
  return resolveResetCycleChoice(resetGroups(nowMs), selection, nowMs);
}

async function fetchResetCycleContext(signal, selection = null) {
  const params = new URLSearchParams();
  if (selection?.providerKey) {
    const [node, provider] = JSON.parse(selection.providerKey);
    params.set("node_id", node); params.set("provider_id", provider);
  }
  const suffix = params.size ? `?${params}` : "";
  const currentParams = new URLSearchParams(params);
  currentParams.set("include_history", "false");
  const [history, current] = await Promise.all([
    fetchJson(`/v3/dashboard/quota/resets${suffix}`, signal),
    fetchJson(`/v3/dashboard/quota?${currentParams}`, signal),
  ]);
  return { ...history, currentProviders: current.providers };
}

function selectedRange(nowMs = Date.now()) {
  const range = state.rangePreset === "all"
    ? resolveAllTimeRange(state.firstRecordedAt, nowMs)
    : state.rangePreset === "last-reset"
      ? resolveLastResetChoice(nowMs)?.range
      : resolvePresetRange(state.rangePreset, nowMs, state.customRange);
  if (!range) {
    const key = state.lastResetSelection?.mode === "last" ? "filters.lastCycleUnavailable" : "filters.lastResetUnavailable";
    const error = new Error(t(key));
    error.translationKey = key;
    throw error;
  }
  if (!Number.isFinite(range.from) || !Number.isFinite(range.to) || range.from >= range.to) {
    const error = new Error(t("error.invalidRange"));
    error.translationKey = "error.invalidRange";
    throw error;
  }
  return range;
}

function selectedRangeIncludesNow() {
  const nowMs = Date.now();
  const range = selectedRange(nowMs);
  return rangeIncludesNow(range, nowMs);
}

function comparisonPredictionId() {
  const selection = state.comparison.selection;
  return selection?.providerKey && selection?.tierId
    ? JSON.stringify([...JSON.parse(selection.providerKey), ...JSON.parse(selection.tierId)]) : null;
}

function setQuotaPrediction(key, checked) {
  if (checked) state.quotaPredictions.add(key); else state.quotaPredictions.delete(key);
  if (state.comparison.enabled && key === comparisonPredictionId()) {
    state.comparison.predictDefaultPending = null;
    if (checked) state.comparison.estimatedQuota = true;
  }
}

function paramsForRange(range, includeFilters = true) {
  const params = new URLSearchParams({
    from: String(range.from),
    to: String(range.to),
    tz_offset_minutes: String(-new Date().getTimezoneOffset()),
  });
  if (state.rangePreset === "all") params.set("all_time", "true");
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

function applyDashboardSettings(settings) {
  state.settings = settings;
  if (state.quotaSelection === undefined) state.quotaSelection = structuredClone(settings.quotaDefaults?.providers ?? null);
  if (state.quotaSelection === undefined) {
    state.quotaSelection = structuredClone(settings?.quotaDefaults?.providers ?? null);
  }
  const defaults = settings?.dashboardDefaults || {};
  applyModelBillingMultipliers(settings);
  state.rangePreset = RANGE_PRESETS.includes(defaults.rangePreset) ? defaults.rangePreset : "24h";
  state.timeFormat = defaults.timeFormat === "12h" ? "12h" : "24h";
  state.lastResetSelection = defaults.lastReset
    ? {
      providerKey: providerIdentity(defaults.lastReset),
      tierId: metricIdentity({
        key: defaults.lastReset.metricKey,
        kind: defaults.lastReset.metricKind,
        unit: defaults.lastReset.unit,
      }),
    }
    : null;
  state.settingsPromise = Promise.resolve(settings);
}

function applyModelBillingMultipliers(settings) {
  const multipliers = Array.isArray(settings?.dashboardDefaults?.modelBillingMultipliers)
    ? settings.dashboardDefaults.modelBillingMultipliers
    : [];
  state.modelBillingMultipliers = multipliers;
  renderCostAdjustment();
}

async function preloadQuotaForLastReset(includeHistory = false) {
  const range = resolvePresetRange("24h");
  const params = paramsForRange(range, false);
  params.set("bucket", state.chartBucket);
  params.set("include_history", String(includeHistory));
  const response = await fetchJson(`/v3/dashboard/quota?${params}`);
  state.quota = response;
  quotaSnapshots.invalidate();
  return response;
}

function baseParams(includeFilters = true) {
  return paramsForRange(selectedRange(), includeFilters);
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

function setSelectOptions(select, values, allLabel, preserveSelection = false) {
  const previous = select.value;
  const previousLabel = select.selectedOptions[0]?.textContent || previous;
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
  if (preserveSelection && previous && ![...select.options].some((option) => option.value === previous)) {
    const option = document.createElement("option");
    option.value = previous;
    option.textContent = previousLabel;
    select.append(option);
  }
  if ([...select.options].some((option) => option.value === previous)) {
    select.value = previous;
  }
}

async function refreshFilters(signal, range = selectedRange()) {
  const params = paramsForRange(range, false);
  const filters = await fetchJson(`/v3/dashboard/filters?${params}`, signal);
  setSelectOptions(elements.nodeFilter, filters.nodes, t("filters.allNodes"), ["last-reset", "all"].includes(state.rangePreset));
  setSelectOptions(elements.appFilter, filters.apps, t("filters.allApps"), ["last-reset", "all"].includes(state.rangePreset));
  setSelectOptions(elements.providerFilter, filters.providers, t("filters.allProviders"), ["last-reset", "all"].includes(state.rangePreset));
  setSelectOptions(elements.modelFilter, filters.models, t("filters.allModels"), ["last-reset", "all"].includes(state.rangePreset));
  setSelectOptions(elements.sourceFilter, filters.dataSources, t("filters.allSources"), ["last-reset", "all"].includes(state.rangePreset));
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

function renderCostAdjustment() {
  const notes = [];
  const unavailable = Number(state.overview?.summary?.unadjustedCostRequests || 0);
  if (unavailable) notes.push(t("kpi.unadjustedCost", { count: formatters.integerNumber.format(unavailable) }));
  elements.kpiCostAdjustment.hidden = !notes.length;
  elements.kpiCostAdjustment.textContent = notes.join(" · ");
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
  animateMetric($("kpiCacheRate"), summary.cacheHitRate * 100, (value) => `${value.toFixed(2)}%`);
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

function comparisonSeries() {
  const selection = state.comparison.selection;
  const providers = (state.quota?.providers || []).filter((provider) =>
    provider.series?.some((series) => series.points.some((point) => quotaPercentage(point) != null)));
  setResetSelectOptions($("comparisonProvider"), [
    { value: "", label: t("compare.chooseProvider") },
    ...providers.map((provider) => ({ value: providerIdentity(provider),
      label: `${provider.nodeName || provider.nodeId} / ${quotaDisplayName(provider)}` })),
  ], selection?.providerKey || "");
  const provider = providers.find((item) => providerIdentity(item) === selection?.providerKey);
  const series = (provider?.series || []).filter((item) => item.points.some((point) => quotaPercentage(point) != null));
  setResetSelectOptions($("comparisonTier"), [
    { value: "", label: t("compare.chooseTier") },
    ...series.map((item) => ({ value: metricIdentity(item), label: item.label || item.key })),
  ], selection?.tierId || "");
  $("comparisonTier").disabled = !provider;
  const selected = series.find((item) => metricIdentity(item) === selection?.tierId);
  return selected ? { ...selected, provider } : null;
}

function historyContext(series = comparisonSeries()) {
  if (!series || !state.overview?.range) return null;
  return { provider: series.provider, metric: series, range: state.overview.range,
    params: paramsForRange(state.overview.range), billing: state.modelBillingMultipliers };
}

function historyDescription(reference) {
  const lines = [[t("history.line"), `${formatters.moneyNumber.format(reference.amount)} = 100%`]];
  if (reference.mode !== "manual") {
    lines.push([t("history.period"), `${formatters.quotaDateTime.format(reference.from * 1000)} – ${formatters.quotaDateTime.format(reference.to * 1000)}`],
      [t("history.sample"), `${formatters.quotaDateTime.format(reference.sampledAt * 1000)} · ${formatPercent(reference.utilizationPercent)}`],
      [t("compare.cycleCost"), `${formatters.moneyNumber.format(reference.cost)}`]);
  }
  return lines;
}

function historyStatus(result) {
  return t(`history.${result?.loading ? "loading" : result?.reason || "unavailable"}`);
}

function renderHistoryDialog() {
  if (!historyDraft || !dialogIsOpen($("historyReferenceDialog"))) return;
  const context = historyContext();
  const result = historyDraft.mode === "none" ? { reason: "disabled" }
    : context ? historyPreviewLoader.ensure(context, historyDraft) : null;
  if (historyDraft.mode === "none") historyPreviewLoader.cancel();
  $("historyReferenceSource").value = historyDraft.mode;
  $("historyReferenceAmountField").hidden = historyDraft.mode !== "manual";
  $("historyReferenceCycleField").hidden = historyDraft.mode !== "cycle";
  setResetSelectOptions($("historyReferenceCycle"), [
    { value: "", label: t("history.chooseCycle") },
    ...(result?.cycles || []).map((cycle) => ({ value: String(cycle.resetsAt),
      label: `${formatters.quotaDateTime.format(cycle.from * 1000)} – ${formatters.quotaDateTime.format(cycle.to * 1000)}` })),
  ], historyDraft.identity === (context && historyIdentity(context)) ? String(historyDraft.cycleId || "") : "");
  $("historyReferenceApply").disabled = historyDraft.mode !== "none" && !result?.reference;
  $("historyReferenceRetry").hidden = result?.reason !== "failed";
  $("historyReferencePreview").textContent = result?.reference
    ? historyDescription(result.reference).map(([label, value]) => `${label}: ${value}`).join("\n") : historyStatus(result);
  positionDialog($("historyReferenceDialog"), $("historyReferenceEdit"));
}

function openHistoryDialog() {
  historyDraft = { ...state.comparison.historyConfig };
  $("historyReferenceAmount").value = historyDraft.amount ?? "";
  historyPreviewLoader.cancel();
  openDialog($("historyReferenceDialog"), $("historyReferenceEdit"));
  renderHistoryDialog();
}

function closeHistoryDialog() {
  closeDialog($("historyReferenceDialog"), $("historyReferenceEdit"));
  historyDraft = null;
  historyPreviewLoader.cancel();
}

function renderTrend() {
  elements.trendMetric.title = elements.trendMetric.selectedOptions[0]?.textContent || "";
  const comparing = state.comparison.enabled;
  if (comparing && (state.resources.overview || state.resources.quota)) return;
  $("estimatedQuotaStatus").hidden = true;
  elements.trendChart.classList.toggle("is-comparing", comparing);
  $("trendCompare").checked = comparing;
  $("comparisonControls").hidden = !comparing;
  $("comparisonStatus").hidden = !comparing;
  elements.trendCumulative.disabled = false;
  elements.trendMetric.querySelector('[value="avgLatencyMs"]').disabled = comparing;
  const quotaSeries = comparing ? comparisonSeries() : null;
  const predictionId = quotaSeries ? JSON.stringify([quotaSeries.provider.nodeId,
    quotaSeries.provider.providerId, quotaSeries.key, quotaSeries.kind, quotaSeries.unit || ""]) : null;
  if (comparing && state.comparison.predictDefaultPending !== null && predictionId) {
    // Apply entry defaults once; subsequent renders must preserve manual choices.
    const checked = state.comparison.predictDefaultPending;
    state.comparison.predictDefaultPending = null;
    if (checked) state.quotaPredictions.add(predictionId); else state.quotaPredictions.delete(predictionId);
    if (state.quota) {
      renderQuota();
      return;
    }
  }
  $("comparisonPredict").disabled = !quotaSeries;
  $("comparisonPredict").checked = state.quotaPredictions.has(predictionId);
  $("comparisonEstimatedQuota").disabled = !quotaSeries;
  $("comparisonEstimatedQuota").checked = state.comparison.estimatedQuota;
  const historyContextValue = comparing && quotaSeries ? historyContext(quotaSeries) : null;
  const historyResult = historyContextValue && state.comparison.historyEnabled
    ? historyQuotaLoader.ensure(historyContextValue, state.comparison.historyConfig) : null;
  if (!historyContextValue || !state.comparison.historyEnabled) historyQuotaLoader.cancel();
  const referenceLabel = !state.comparison.historyEnabled ? t("history.none")
    : historyResult?.reference ? formatters.moneyNumber.format(historyResult.reference.amount)
    : historyResult?.loading ? "…" : "—";
  $("historyReferenceEdit").textContent = `${t("compare.historyLabel")}: ${referenceLabel}`;
  $("historyReferenceEdit").title = historyResult?.reference
    ? historyDescription(historyResult.reference).map(([label, value]) => `${label}: ${value}`).join("\n")
    : !state.comparison.historyEnabled ? t("history.disabled") : historyStatus(historyResult);
  $("historyReferenceEdit").setAttribute("aria-label", `${t("history.configure")}: ${$("historyReferenceEdit").title}`);
  renderHistoryDialog();
  const matchingRange = state.quota?.range?.from === state.overview?.range?.from
    && state.quota?.range?.to === state.overview?.range?.to;
  // While a new range loads, retain the previous chart until both responses
  // arrive. Never draw an old quota range against new Usage data.
  if (comparing && !matchingRange) {
    estimatedQuotaLoader.cancel();
    return;
  }
  if (comparing) {
    $("comparisonStatus").textContent = t(!quotaSeries ? "compare.select" : "compare.waiting");
    $("comparisonStatus").hidden = !!quotaSeries && matchingRange;
  }
  const rawPoints = state.overview?.trend || [];
  const metric = elements.trendMetric.value;
  const cumulativeSupported = ["realTotalTokens", "totalRequests", "totalCostUsd"].includes(metric);
  elements.trendCumulativeToggle.hidden = !cumulativeSupported;
  if (!cumulativeSupported) {
    state.trendCumulative = false;
    elements.trendCumulative.checked = false;
  } else {
    elements.trendCumulative.checked = state.trendCumulative;
  }
  const points = state.trendCumulative && cumulativeSupported
    ? accumulateTrendPoints(rawPoints)
    : rawPoints;
  elements.trendEmpty.hidden = rawPoints.length > 0;
  if (state.overview?.range?.bucket) {
    elements.resolvedBucket.textContent = t("trend.resolvedBucket", {
      bucket: state.overview.range.bucket,
    });
  }
  if (!points.length) {
    estimatedQuotaLoader.cancel();
    clearChart("trend", elements.trendChart);
    return;
  }

  const predictionEnabled = comparing && matchingRange && quotaSeries && state.quotaPredictions.has(predictionId);
  const predictionState = predictionEnabled ? resolveQuotaPrediction(quotaSeries, quotaPercentage) : {};
  if (predictionState.prediction) predictionChartCache.set(predictionId, predictionState.prediction);
  else if (predictionState.history?.loading) predictionState.prediction = predictionChartCache.get(predictionId);
  if (predictionEnabled && !predictionState.prediction) {
    $("comparisonStatus").hidden = false;
    $("comparisonStatus").textContent = t(predictionState.history?.loading ? "quota.predictLoading" : "quota.predictUnavailable");
  }
  let quotaEstimate = null;
  const estimateEnabled = comparing && matchingRange && quotaSeries && state.comparison.estimatedQuota;
  if (estimateEnabled) {
    const params = paramsForRange(state.overview.range);
    params.delete("all_time");
    const scope = new URLSearchParams(params);
    scope.delete("from"); scope.delete("to");
    const key = JSON.stringify([predictionId, scope.toString(), state.overview.range.bucket,
      state.rangePreset, state.rangePreset === "custom" ? state.customRange : null, state.modelBillingMultipliers]);
    quotaEstimate = estimatedQuotaLoader.ensure(key, { provider: quotaSeries.provider,
      metric: quotaSeries, overview: state.overview, params });
  } else {
    estimatedQuotaLoader.cancel();
  }
  $("estimatedQuotaStatus").hidden = !estimateEnabled || (!quotaEstimate?.error && Boolean(quotaEstimate?.points.some((point) => point.value != null)));
  $("estimatedQuotaStatus").textContent = t(quotaEstimate?.loading || (estimateEnabled && state.requestController)
    ? "compare.estimateLoading" : quotaEstimate?.error ? "compare.estimateFailed" : "compare.estimateUnavailable");
  const range = comparing ? quotaPredictionRange(state.overview.range, [predictionState.prediction]) : state.overview.range;
  const spanSeconds = Math.max(1, Number(range.to) - Number(range.from));
  const palette = chartPalette();
  const option = (comparing ? buildComparisonOption : buildTrendOption)({
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
    usageRange: state.overview.range,
    estimatedQuotaEnabled: comparing && state.comparison.estimatedQuota,
    historyReferenceEnabled: comparing && state.comparison.historyEnabled,
    historyReference: historyResult?.reference ? { ...historyResult.reference,
      name: t("history.line"), color: cssColor("--success", "#34d399") } : null,
    formatHistoryReferenceTooltip: (reference) => tooltipMarkup(t("history.line"), historyDescription(reference)),
    quotaPlot: comparing && matchingRange && quotaSeries ? {
      id: "comparison-quota", name: `${quotaSeries.label || quotaSeries.key} · %`,
      axis: "percent", color: cssColor("--violet", "#a78bfa"), value: quotaPercentage,
      segments: splitQuotaPoints(quotaSeries.points, state.quota.range.bucketSeconds, quotaPercentage),
      prediction: predictionState.prediction,
    } : null,
    estimatedQuotaPlot: quotaEstimate?.points.some((point) => point.value != null) ? {
      name: t("quota.estimatedQuota"), color: cssColor("--amber", "#fbbf24"), points: quotaEstimate.points,
    } : null,
    formatMoney: (value) => formatters.moneyNumber.format(value),
    formatEstimatedQuotaTooltip: (point) => tooltipMarkup(formatters.quotaDateTime.format(point.at * 1000), [
      [t("quota.estimatedQuota"), formatters.moneyNumber.format(point.value)],
      [t("compare.cycleCost"), formatters.moneyNumber.format(point.cost)],
      [t("quota.percentAxis"), formatPercent(point.utilization)],
      [t("compare.sampledAt"), formatters.quotaDateTime.format(point.sampledAt * 1000)],
      [t("compare.cycleStart"), formatters.quotaDateTime.format(point.cycleFrom * 1000)],
    ]),
    usageLabel: elements.trendMetric.selectedOptions[0].textContent,
    quotaLabel: t("quota.percentAxis"),
    formatPredictionTooltip: (point) => tooltipMarkup(formatters.quotaDateTime.format(point.value[0]), [
      [t("quota.predicted"), formatPercent(point.value[1])],
    ]),
    formatQuotaTooltip: (point) => tooltipMarkup(formatters.quotaDateTime.format(point.sampledAt * 1000), [
      [quotaSeries?.label || quotaSeries?.key || "Quota", formatPercent(quotaPercentage(point))],
    ]),
  });
  updateChart("trend", elements.trendChart, option);
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
  if (point.unadjustedCostRequests) lines.push(["", t("kpi.unadjustedCost", { count: formatters.integerNumber.format(point.unadjustedCostRequests) })]);
  return usageTooltipMarkup(point, titleText, lines, formatTokens);
}

function quotaTooltip(params, ticket, callback) {
  const items = Array.isArray(params) ? params : [params];
  const raw = items.find((item) => item?.axisValue != null || item?.value?.[0] != null);
  if (!raw) return "";
  const chartOption = chartInstances.quota?.getOption();
  // ECharts tooltip params retain the nearest sample even with an unsnapped pointer.
  const pointer = chartOption?.xAxis?.[0]?.axisPointer;
  const at = Math.floor(Number(pointer?.status === "show" && pointer.value != null
    ? pointer.value : raw.axisValue ?? raw.value[0]) / 1000);
  if (!Number.isFinite(at)) return "";
  const title = formatters.quotaDateTime.format(at * 1000);
  const legend = chartOption?.legend?.[0]?.selected || {};
  const plots = state.quotaPlots;
  const predictionLines = () => quotaPredictionRows(plots, at, legend).map(({ plot, value }) => [
    `${plot.name} · ${t("quota.predicted")}`,
    plot.axis === "amount" ? formatTokens(value) + (plot.unit ? " " + plot.unit : "") : Number(value).toFixed(1) + "%",
  ]);
  if (at > Number(state.quota.range.to)) {
    quotaSnapshots.cancel();
    const lines = predictionLines();
    return tooltipMarkup(title, lines.length ? lines : [[t("quota.predicted"), t("quota.noData")]]);
  }
  const markup = (snapshot, placeholder = null) => tooltipMarkup(title,
    quotaSnapshotRows(plots, snapshot || { at, metrics: [] }, legend).map(({ plot, point, value }) => [
      plot.name,
      placeholder || (value == null ? t("quota.noData") :
        (plot.axis === "amount" ? formatTokens(value) + (plot.unit ? " " + plot.unit : "") : Number(value).toFixed(1) + "%")
        + " · " + formatters.quotaDateTime.format(point.sampledAt * 1000)),
    ]).concat(predictionLines()));
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
    appendCell(row, formatters.moneyNumber.format(item.totalCostUsd)
      + (item.unadjustedCostRequests ? ` · ${t("cost.unadjusted")}` : ""), "table-value");
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
  renderCostAdjustment();
  if (state.quota) renderQuotaCards(quotaProviders());
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
  elements.quotaNodeFilter.title = elements.quotaNodeFilter.selectedOptions[0]?.textContent || "";
  const selected = state.quotaSelection;
  const pickerLabels = { allLabel: t("quota.selectAll"), noneLabel: t("quota.selectNone") };
  renderPicker(elements.quotaProviderFilter, {
    ...pickerLabels, title: t("quota.provider"), showTitleInTrigger: false,
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
    ...pickerLabels, title: t("quota.metric"), groups, showTitleInTrigger: false,
    itemControl: {
      label: t("quota.predict"),
      selected: state.quotaPredictions,
      onChange: (key, checked) => {
        setQuotaPrediction(key, checked);
        quotaSnapshots.cancel();
        renderQuotaChart(quotaProviders());
      },
    },
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
  return value;
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
    if (provider.diagnosticCode === "cli_schema_incompatible") {
      status.textContent = t("quota.cliIncompatible");
      status.title = t("quota.cliIncompatibleHint");
    }
    if (provider.diagnosticCode === "quota_api_unavailable") {
      status.textContent = t("quota.apiUnavailable");
      status.title = t("quota.apiUnavailableHint");
    }
    heading.append(title, status);
    card.append(heading);

    const metrics = (provider.current || []).filter(
      (metric) => metricSelected(state.quotaSelection, provider, metric),
    );
    const details = document.createElement("div");
    details.className = `quota-details${metrics.length === 1 ? " is-single" : ""}`;
    if (metrics.length) {
      for (const metric of metrics) {
        const group = document.createElement("div");
        group.className = "quota-metric-group";
        const metricLabel = document.createElement("h3");
        metricLabel.className = "quota-metric-label";
        metricLabel.textContent = metric.label || metric.key;
        const metricValue = document.createElement("strong");
        metricValue.className = "quota-metric-value";
        metricValue.textContent = quotaValueLabel(metric);
        group.append(metricLabel, metricValue);
        const percentage = quotaPercentage(metric);
        if (percentage != null) {
          const meter = document.createElement("div");
          meter.className = "quota-meter";
          meter.setAttribute("role", "progressbar");
          meter.setAttribute("aria-label", metric.label || metric.key);
          meter.setAttribute("aria-valuemin", "0");
          meter.setAttribute("aria-valuemax", "100");
          meter.setAttribute("aria-valuenow", String(percentage));
          const fill = document.createElement("span");
          fill.style.width = `${percentage}%`;
          meter.append(fill);
          group.append(meter);
        } else {
          group.classList.add("quota-metric-amount");
        }
        const reset = optionalQuotaNumber(metric.resetsAt);
        if (reset != null && reset > 0 && Number.isFinite(new Date(reset * 1000).getTime())) {
          appendQuotaDetail(group, t("quota.reset"), formatters.quotaDateTime.format(reset * 1000));
        }
        let estimate = quotaExhaustion(metric, null);
        if (estimate) {
          if (estimate.status !== "exhausted") {
            const series = provider.series?.find((item) => quotaMetricIdentity(item) === quotaMetricIdentity(metric));
            const { prediction, history } = resolveQuotaPrediction({ ...metric, points: series?.points || [], provider });
            estimate = quotaExhaustion(metric, prediction, history || {});
          }
          const text = estimate.status === "estimated"
            ? formatters.quotaDateTime.format(estimate.at * 1000)
            : estimate.status === "unavailable" ? "—"
              : estimate.status === "loading" ? "…" : t(`quota.exhaustion.${estimate.status}`);
          const value = appendQuotaDetail(group, t("quota.estimatedExhaustion"), text);
          if (estimate.status === "unavailable" || estimate.status === "loading") {
            value.title = t(`quota.exhaustion.${estimate.status}`);
            value.setAttribute("aria-label", value.title);
          }
          value.classList.toggle("quota-exhaustion-early", estimate.beforeReset);
          value.parentElement.classList.add("quota-exhaustion");
          value.parentElement.dataset.status = estimate.status;
          const quota = estimatedQuota(metric, state.overview?.summary?.totalCostUsd);
          appendQuotaDetail(group, t("quota.estimatedQuota"),
            quota == null ? "—" : formatters.moneyNumber.format(quota));
        }
        details.append(group);
      }
    } else {
      appendQuotaDetail(details, t("quota.current"), t("quota.noSuccessfulSample"));
    }
    const footer = document.createElement("div");
    footer.className = "quota-card-footer";
    appendQuotaDetail(
      footer,
      t("quota.lastSuccess"),
      provider.lastSuccessAt == null
        ? "—"
        : formatters.quotaDateTime.format(provider.lastSuccessAt * 1000),
    );
    appendQuotaDetail(
      footer,
      t("quota.lastCheck"),
      formatters.quotaDateTime.format(provider.checkedAt * 1000),
    );
    card.append(details, footer);
    elements.quotaCards.append(card);
  }
}

function quotaSeries(providers) {

  return providers.flatMap((provider) => (provider.series || [])
    .filter((series) => metricSelected(state.quotaSelection, provider, series))
    .map((series) => ({ ...series, provider }))
    .filter((series) => series.points?.length));
}

function predictionHistory(item, metricId) {
  const range = state.quota.range;
  const from = quotaPredictionLookback(item, range);
  if (from >= Number(range.from)) return null;
  const key = JSON.stringify([metricId, from, range.to, range.bucketSeconds]);
  const existing = state.quotaPredictionHistory.get(key);
  if (existing) return existing;
  const controller = new AbortController();
  const entry = { loading: true, points: [], controller };
  state.quotaPredictionHistory.set(key, entry);
  const quota = state.quota;
  const params = new URLSearchParams({
    from: String(from), to: String(range.to), bucket: `${range.bucketSeconds}s`,
    node_id: item.provider.nodeId, provider_id: item.provider.providerId,
  });
  fetchJson(`/v3/dashboard/quota?${params}`, controller.signal).then((response) => {
    const provider = response.providers?.find((provider) => providerIdentity(provider) === providerIdentity(item.provider));
    entry.points = provider?.series?.find((series) => quotaMetricIdentity(series) === quotaMetricIdentity(item))?.points || [];
  }).catch((error) => {
    if (error.name !== "AbortError") entry.error = error;
  }).finally(() => {
    entry.loading = false;
    if (state.quota === quota && !controller.signal.aborted) {
      const providers = quotaProviders();
      renderQuotaCards(providers);
      renderQuotaChart(providers);
    }
  });
  return entry;
}

function resolveQuotaPrediction(item, valueSelector) {
  const metricId = JSON.stringify([item.provider.nodeId, item.provider.providerId, item.key, item.kind, item.unit || ""]);
  const signature = JSON.stringify([state.quota.range.bucketSeconds, item.points]);
  const cached = resolvedPredictionCache.get(metricId);
  if (cached?.signature === signature && !cached.result.history?.loading
    && (cached.result.prediction || !cached.result.history?.points.length)) return cached.result;
  let prediction = quotaPrediction(item.points, state.quota.range.bucketSeconds, valueSelector);
  const history = !prediction ? predictionHistory(item, metricId) : null;
  if (history?.points.length) prediction = quotaPrediction(history.points, state.quota.range.bucketSeconds, valueSelector);
  const result = { prediction, history };
  resolvedPredictionCache.set(metricId, { signature, result });
  return result;
}

function renderQuotaChart(providers) {
  if (state.resources.quota && chartInstances.quota) return;
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
    if (state.comparison.enabled) renderTrend();
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
  const plots = plottedSeries.map((item) => {
    const axisLabel = item.axis === "percent" ? "%" : item.unit || t("quota.amountAxis");
    const metricId = JSON.stringify([item.provider.nodeId, item.provider.providerId, item.key, item.kind, item.unit || ""]);
    const enabled = state.quotaPredictions.has(metricId);
    let { prediction, history } = enabled
      ? resolveQuotaPrediction(item, item.value) : { prediction: null, history: null };
    if (prediction) predictionChartCache.set(metricId, prediction);
    else if (history?.loading) {
      const previous = predictionChartCache.get(metricId);
      if (previous?.start.at >= state.quota.range.from && previous.start.at <= state.quota.range.to) prediction = previous;
    }
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
      metricId,
      prediction,
      predictionPending: history?.loading || false,
      predictionEnabled: enabled,
      color: colors[item.seriesIndex % colors.length],
      value: item.value,
      segments: splitQuotaPoints(
        item.points,
        state.quota.range.bucketSeconds,
        item.value,
      ),
    };
  });
  const range = quotaPredictionRange(state.quota.range, plots.map((plot) => plot.prediction));
  const predictionStatus = $("quotaPredictionStatus");
  const pending = plots.some((plot) => plot.predictionPending);
  const unavailable = plots.some((plot) => plot.predictionEnabled && !plot.prediction);
  predictionStatus.hidden = !pending && !unavailable;
  predictionStatus.textContent = pending ? t("quota.predictLoading") : unavailable ? t("quota.predictUnavailable") : "";
  const spanSeconds = Math.max(1, Number(range.to) - Number(range.from));
  for (const plot of plots) {
    if (plot.axis === "amount" && plot.prediction) amountValues.push(plot.prediction.end.value);
  }
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
  if (state.comparison.enabled) renderTrend();
}

function renderQuota() {
  quotaSnapshots.cancel();
  chartInstances.quota?.dispatchAction({ type: "hideTip" });
  updateQuotaFilters();
  if (dialogIsOpen(elements.rangePickerDialog)) updateRangePickerMode();
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
    const modelCell = appendCell(row, `${item.providerName || item.providerId || "—"} / ${item.model || item.requestModel || "—"}`);
    const metadata = document.createElement("div"); metadata.className = "event-metadata";
    for (const [key, value] of [["tier", item.serviceTier], ["effort", item.reasoningEffort]]) {
      const tag = document.createElement("span");
      tag.textContent = `${t(`events.${key}`)}: ${value === "default" ? "Standard" : value || t("events.unknown")}`;
      if (key === "tier") {
        tag.title = t(item.serviceTierSource === "response" ? "events.metadataResponse" : item.serviceTierSource === "request" ? "events.metadataRequest" : "events.metadataUnknown");
        tag.tabIndex = 0;
        tag.setAttribute("aria-label", `${tag.textContent}; ${tag.title}`);
      }
      metadata.append(tag);
    }
    modelCell.append(metadata);
    appendCell(row, formatTokens(item.realTotalTokens), "table-value");
    appendCell(row, formatTokens(item.cacheReadTokens), "table-value");
    appendCell(row, formatters.moneyNumber.format(item.totalCostUsd)
      + (item.unadjustedCostRequests ? ` · ${t("cost.unadjusted")}` : ""), "table-value");
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

async function loadEvents({ append = false, signal = undefined, range = state.activeRange || selectedRange() } = {}) {
  if (append && state.eventsLoading) return;
  state.eventsController?.abort();
  const controller = new AbortController(); state.eventsController = controller;
  const abort = () => controller.abort();
  if (signal?.aborted) abort();
  signal?.addEventListener("abort", abort, { once: true });
  const generation = ++state.eventsGeneration;
  state.eventsLoading = true;
  elements.loadMore.disabled = true;
  try {
    const params = paramsForRange(range, true);
    params.set("limit", "50");
    if ($("eventTierFilter").value) params.set("service_tier", $("eventTierFilter").value);
    if ($("eventEffortFilter").value) params.set("reasoning_effort", $("eventEffortFilter").value);
    if (append && state.eventCursor) {
      params.set("before_created_at", String(state.eventCursor.beforeCreatedAt));
      params.set("before_event_id", state.eventCursor.beforeEventId);
    }
    const response = await fetchJson(`/v3/dashboard/events?${params}`, controller.signal);
    if (controller.signal.aborted || generation !== state.eventsGeneration) return;
    state.events = append ? [...state.events, ...response.items] : response.items;
    renderEventRows(state.events, false);
    state.eventCursor = response.nextCursor;
    elements.eventsEmpty.hidden = state.events.length > 0;
    elements.loadMore.hidden = !state.eventCursor;
  } finally {
    signal?.removeEventListener("abort", abort);
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
    delete state.resources[name];
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

function refreshOverview(parent, range = selectedRange()) {
  const params = paramsForRange(range, true);
  params.set("bucket", state.chartBucket === "auto" && state.comparison.enabled ? "quota-auto" : state.chartBucket);
  return resource("overview", (signal) => fetchJson(`/v3/dashboard/overview?${params}`, signal), renderOverview, parent);
}

function refreshQuota(parent, range = selectedRange()) {
  const params = paramsForRange(range, false);
  params.set("bucket", state.chartBucket);
  return resource("quota", async (signal) => {
    const response = await fetchJson(`/v3/dashboard/quota?${params}`, signal);
    if (state.quotaSelection === undefined) await state.settingsPromise;
    return response;
  }, (response) => {
    if (state.quotaPredictionHistory.size > 64) {
      for (const entry of state.quotaPredictionHistory.values()) entry.controller.abort();
      state.quotaPredictionHistory.clear(); resolvedPredictionCache.clear();
    }
    state.quota = response;
      quotaSnapshots.invalidate();
    renderQuota();
  }, parent);
}

async function refreshAll({ reloadFilters = false, revalidate = false, reason = "refresh" } = {}) {
  if (revalidate) {
    estimatedQuotaLoader.revalidate();
    historyQuotaService.invalidate();
    historyQuotaLoader.cancel(); historyPreviewLoader.cancel();
    resolvedPredictionCache.clear();
    for (const entry of state.quotaPredictionHistory.values()) entry.controller.abort();
    state.quotaPredictionHistory.clear();
  }
  const previousOverview = state.overview, previousQuota = state.quota;
  state.requestController?.abort();
  const controller = new AbortController();
  state.requestController = controller;
  // Invalidate independently started requests when the shared range changes.
  for (const pending of Object.values(state.resources)) pending.abort();
  elements.refreshButton.disabled = true;
  setConnection("", "status.refreshing");
  try {
    if (reason !== "filters" && state.rangePreset === "all") {
      const bounds = await fetchJson("/v3/dashboard/time-bounds", controller.signal);
      if (controller.signal.aborted) return;
      state.firstRecordedAt = bounds.firstRecordedAt;
    } else if (reason !== "filters" && state.rangePreset === "last-reset"
      && (reason !== "range" || !state.resetCycleHistory.response)) {
      const response = await fetchResetCycleContext(controller.signal, state.lastResetSelection);
      if (controller.signal.aborted) return;
      state.resetCycleHistory = { loading: false, error: false, response };
      const choice = resolveLastResetChoice();
      if (choice.selection) state.lastResetSelection = choice.selection;
    }

    if (reason === "range" && state.comparison.enabled) {
      // A real range selection supersedes entry defaults; refresh/filter changes do not.
      state.comparison.predictDefaultPending = null;
      if (!selectedRangeIncludesNow()) {
        const key = comparisonPredictionId();
        if (key) setQuotaPrediction(key, false);
        else state.comparison.predictDefaultPending = false;
        $("comparisonPredict").checked = false;
      }
    }
    const chartRange = reason === "filters" && state.activeRange ? state.activeRange : selectedRange();
    state.activeRange = chartRange;
    if (reloadFilters) await refreshFilters(controller.signal, chartRange);
    if (controller.signal.aborted) return;
    if (reason === "refresh" || !state.settings) state.settingsPromise = resource("settings", (signal) => fetchJson("/v3/dashboard/settings", signal), (settings) => {
      const changed = JSON.stringify(state.settings) !== JSON.stringify(settings);
      applyModelBillingMultipliers(settings);
      state.settings = settings;
      if (state.quotaSelection === undefined) state.quotaSelection = structuredClone(settings.quotaDefaults.providers);
      if (changed && state.quota) renderQuota();
    }, controller.signal);
    const quotaMatches = state.quota?.range.from === chartRange.from && state.quota?.range.to === chartRange.to;
    const dailyKey = dailyParams(true).toString();
    const dailyNeeded = reason !== "range" || state.dailyKey !== dailyKey || !state.daily;
    await Promise.allSettled([
      state.settingsPromise,
      refreshOverview(controller.signal, chartRange),
      dailyNeeded ? resource("daily", (signal) => fetchJson(`/v3/dashboard/daily?${dailyKey}`, signal), (daily) => {
        state.daily = daily; state.dailyKey = dailyKey; renderDaily();
      }, controller.signal) : Promise.resolve(),
      reason !== "filters" || !quotaMatches ? refreshQuota(controller.signal, chartRange) : Promise.resolve(),
      resource("events", (signal) => loadEvents({ append: false, signal, range: chartRange }), () => {}, controller.signal),
    ]);
    if (controller.signal.aborted) return;
    if (state.resourceErrors.size) throw state.resourceErrors.values().next().value;
    clearError();
    setConnection("online", "status.online");
    state.updatedAt = Date.now();
    elements.updatedAt.textContent = t("status.updatedAt", { time: formatters.dateTime.format(state.updatedAt) });
  } catch (error) {
    if (state.comparison.enabled && state.requestController === controller && (state.resourceErrors.has("overview") || state.resourceErrors.has("quota"))) {
      state.overview = previousOverview; state.quota = previousQuota;
    }
    if (error.name !== "AbortError") showError(error);
  } finally {
    if (state.requestController === controller) {
      state.requestController = null;
      if (state.quota) renderQuota();
      if (state.overview) renderTrend();
      elements.refreshButton.disabled = false;
    }
  }
}

async function initializeDashboard() {
  try {
    const settings = await fetchJson("/v3/dashboard/settings");
    applyDashboardSettings(settings);
    if (state.rangePreset === "last-reset") {
      try {
        await preloadQuotaForLastReset();
        let choice = resolveLastResetChoice();
        if (!choice?.range) {
          await preloadQuotaForLastReset(true);
          choice = resolveLastResetChoice();
        }
        if (choice?.range) {
          state.lastResetSelection = {
            providerKey: providerIdentity(choice.group.provider),
            tierId: choice.tier.id,
          };
        } else {
          state.rangePreset = "24h";
        }
      } catch {
        state.rangePreset = "24h";
      }
    }
  } catch {
    // Keep the built-in 24-hour default when settings cannot be loaded.
  }
  updateTimeInputPlaceholders();
  updateRangePickerTrigger();
  await refreshAll({ reloadFilters: true });
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
  if (historyDraft) renderHistoryDialog();
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
    "last-reset": "filters.rangeLastReset",
    all: "filters.rangeAll",
    custom: "filters.custom",
  }[preset] || "filters.range24h";
}

function setResetSelectOptions(select, options, selectedValue) {
  select.replaceChildren();
  for (const item of options) {
    const option = document.createElement("option");
    option.value = item.value;
    option.textContent = item.label;
    select.append(option);
  }
  select.disabled = options.length === 0;
  if (options.some((option) => option.value === selectedValue)) select.value = selectedValue;
}

function pickerPreset() {
  return state.rangePresetDraft ?? state.rangePreset;
}

function renderLastResetEditor() {
  const history = state.resetCycleHistory;
  const groups = resetGroups();
  const choice = resolveResetCycleChoice(groups, state.lastResetDraft);
  if (choice.selection) state.lastResetDraft = choice.selection;
  setResetSelectOptions(elements.lastResetProvider, groups.map(g => ({
    value: g.id, label: `${g.provider.nodeName || g.provider.nodeId} / ${quotaDisplayName(g.provider)}`,
  })), choice.group?.id);
  setResetSelectOptions(elements.lastResetTier, (choice.group?.tiers || []).map(tier => ({
    value: tier.id, label: `${tier.periodLabel} · ${tier.metric.label || tier.metric.key}`,
  })), choice.tier?.id);
  const options = (choice.tier?.cycles || []).map(cycle => ({ value: cycle.id,
    label: `${formatters.quotaDateTime.format(cycle.from * 1000)} → ${formatters.quotaDateTime.format(cycle.endAt * 1000)}${cycle.endAt > Date.now() / 1000 ? ` · ${t("filters.pastResetCurrent")}` : ""}`,
  }));
  if (!choice.cycle) options.unshift({ value: "", label: "—" });
  setResetSelectOptions(elements.resetCycleSelect, options, choice.cycle?.id || "");
  elements.resetCycleSelect.disabled = history.loading || history.error || !choice.tier?.cycles.length;
  for (const [button, mode] of [[elements.resetCycleCurrent, "current"], [elements.resetCycleLast, "last"]]) {
    button.classList.toggle("active", choice.mode === mode);
    button.setAttribute("aria-pressed", String(choice.mode === mode));
  }
  const range = choice.range;
  elements.lastResetFrom.textContent = range ? formatters.quotaDateTime.format(range.from * 1000) : "—";
  elements.lastResetTo.textContent = range ? formatters.quotaDateTime.format(range.to * 1000) : "—";
  const key = history.loading ? "filters.pastResetLoading" : history.error ? "filters.pastResetFailed"
    : !range ? (choice.mode === "last" ? "filters.lastCycleUnavailable" : "filters.lastResetUnavailable") : null;
  elements.lastResetError.textContent = key ? t(key) : "";
  elements.lastResetError.hidden = !key;
  elements.resetCycleRetry.hidden = !history.error;
  elements.applyRange.disabled = history.loading || history.error || !range;
}

function updateRangePickerMode() {
  const reset = pickerPreset() === "last-reset";
  $("customRangeTitle").textContent = t(reset ? "filters.rangeLastReset" : "filters.customTitle");
  $("rangePickerEyebrow").textContent = reset ? "RESET CYCLE" : "CUSTOM RANGE";
  elements.customRangeEditor.hidden = reset;
  for (const input of elements.customRangeEditor.querySelectorAll("input")) input.disabled = reset;
  elements.lastResetEditor.hidden = !reset;
  if (reset) renderLastResetEditor();
  else elements.applyRange.disabled = false;
  if (state.rangePresetController) elements.applyRange.disabled = true;
}

function updateRangePickerTrigger() {
  elements.rangePickerLabel.textContent = t(rangeLabelKey(state.rangePreset));
  for (const button of elements.rangePresetOptions.querySelectorAll("[data-range-preset]")) {
    const active = button.dataset.rangePreset === pickerPreset();
    button.classList.toggle("active", active);
    button.setAttribute("aria-pressed", String(active));
  }
  updateRangePickerMode();
}

function syncRangeInputs(resetMonth = true) {
  const range = state.rangeDraft || defaultCustomRange();
  updateTimeInputPlaceholders();
  elements.customFromDate.value = dateInputValue(range.from);
  elements.customFromTime.value = timeInputValue(range.from, state.timeFormat);
  elements.customToDate.value = dateInputValue(range.to);
  elements.customToTime.value = timeInputValue(range.to, state.timeFormat);
  if (resetMonth) state.rangeCalendarMonth = new Date(new Date(range.from * 1000).getFullYear(), new Date(range.from * 1000).getMonth(), 1);
  renderCalendar();
}

function updateTimeInputPlaceholders() {
  const placeholder = timeInputPlaceholder(state.timeFormat);
  elements.customFromTime.placeholder = placeholder;
  elements.customToTime.placeholder = placeholder;
}

function readRangeInputs() {
  const from = parseDateTimeParts(elements.customFromDate.value, elements.customFromTime.value, state.timeFormat);
  const to = parseDateTimeParts(elements.customToDate.value, elements.customToTime.value, state.timeFormat);
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
      const inputs = readRangeInputs();
      const draft = Number.isFinite(inputs.from) && Number.isFinite(inputs.to) ? inputs : range;
      state.rangeDraft = selectCalendarRange(draft, day, state.activeRangeField);
      state.activeRangeField = state.activeRangeField === "start" ? "end" : "start";
      elements.customRangeError.hidden = true;
      syncRangeInputs(false);
    });
    elements.calendarDays.append(button);
  }
}

function openRangePicker() {
  if (dialogIsOpen(elements.rangePickerDialog)) {
    closeRangePicker();
    return;
  }
  state.rangePresetDraft = state.rangePreset;
  state.lastResetDraft = state.lastResetSelection ? { ...state.lastResetSelection } : null;
  state.rangeDraft = { ...(state.customRange || defaultCustomRange()) };
  state.activeRangeField = "start";
  elements.customRangeError.hidden = true;
  syncRangeInputs();
  updateRangePickerTrigger();
  openDialog(elements.rangePickerDialog, elements.rangePickerTrigger);
  if (pickerPreset() === "last-reset") resetCycleLoader.load();
}

function closeRangePicker(discard = true) {
  state.rangePresetController?.abort();
  state.rangePresetController = null;
  if (discard) state.rangeDraft = null;
  resetCycleLoader.cancel();
  state.rangePresetDraft = null;
  state.lastResetDraft = null;
  closeDialog(elements.rangePickerDialog, elements.rangePickerTrigger);
}

async function setRangePreset(preset) {
  resetCycleLoader.cancel();
  state.rangePresetController?.abort();
  state.rangePresetController = null;
  if (preset === "all") {
    const controller = new AbortController();
    state.rangePresetController = controller;
    const button = elements.rangePresetOptions.querySelector('[data-range-preset="all"]');
    button.disabled = true;
    elements.applyRange.disabled = true;
    try {
      const bounds = await fetchJson("/v3/dashboard/time-bounds", controller.signal);
      if (controller.signal.aborted || !dialogIsOpen(elements.rangePickerDialog)) return;
      state.firstRecordedAt = bounds.firstRecordedAt;
      state.rangePreset = "all";
      closeRangePicker();
      updateRangePickerTrigger();
      refreshAll({ reloadFilters: true, reason: "range" });
    } catch (error) {
      if (error.name !== "AbortError") showError(error);
    } finally {
      button.disabled = false;
      if (state.rangePresetController === controller) state.rangePresetController = null;
      if (dialogIsOpen(elements.rangePickerDialog)) updateRangePickerMode();
    }
    return;
  }
  if (["custom", "last-reset"].includes(preset)) {
    if (preset === "last-reset" && pickerPreset() !== "last-reset") {
      state.lastResetDraft = { ...state.lastResetSelection, mode: "current", cycleId: null, cycle: null };
    }
    state.rangePresetDraft = preset;
    if (preset === "custom") {
      state.rangeDraft = { ...(state.customRange || defaultCustomRange()) };
      state.activeRangeField = "start";
      syncRangeInputs();
    }
    updateRangePickerTrigger();
    if (preset === "last-reset") resetCycleLoader.load();
    return;
  }
  state.rangePreset = preset;
  state.rangeDraft = null;
  closeRangePicker();
  updateRangePickerTrigger();
  refreshAll({ reloadFilters: true, reason: "range" });
}

function bucketTrigger() { return state.bucketTarget === "quota" ? $("quotaBucketTrigger") : elements.trendBucketTrigger; }
function updateBucketPicker() {
  elements.trendBucketLabel.textContent = state.chartBucket === "auto" ? t("trend.bucketAuto") : bucketDisplayLabel(state.chartBucket);
  elements.trendBucketTrigger.title = elements.trendBucketLabel.textContent;
  $("quotaGranularityLabel").textContent = state.chartBucket === "auto" ? t("trend.bucketAuto") : bucketDisplayLabel(state.chartBucket);
  $("quotaBucketTrigger").title = $("quotaGranularityLabel").textContent;
  const bucket = state.chartBucket;
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
  state.chartBucket = bucket;
  updateBucketPicker(); closeBucketPicker();
  const range = state.overview?.range || selectedRange();
  // Both endpoints share the same bucket choice, including Auto.
  Promise.allSettled([refreshOverview(undefined, range), refreshQuota(undefined, range)]);
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
  for (const element of document.querySelectorAll("[data-i18n-title]")) {
    element.title = t(element.dataset.i18nTitle);
  }
  elements.languageToggle.textContent = t("language.switchShort");
  elements.languageToggle.setAttribute("aria-label", t("language.switch"));
  applyTheme();
  updateFilterPlaceholders();
  updateRangePickerTrigger();
  updateBucketPicker();
  renderCalendar();
  renderCostAdjustment();
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

elements.refreshButton.addEventListener("click", () => refreshAll({ reloadFilters: true, revalidate: true }));
elements.rangePickerTrigger.addEventListener("click", openRangePicker);
elements.rangePresetOptions.addEventListener("click", (event) => {
  const button = event.target.closest("[data-range-preset]");
  if (button) setRangePreset(button.dataset.rangePreset);
});
elements.rangePickerForm.addEventListener("submit", (event) => {
  event.preventDefault();
  let range;
  const preset = pickerPreset();
  if (preset === "last-reset") {
    if (state.resetCycleHistory.loading || state.resetCycleHistory.error) return;
    const choice = resolveLastResetChoice(Date.now(), state.lastResetDraft);
    if (!choice.range) { renderLastResetEditor(); return; }
    state.lastResetSelection = structuredClone(choice.selection);
    range = choice.range;
  } else {
    range = readRangeInputs();
    if (!Number.isFinite(range.from) || !Number.isFinite(range.to) || range.from >= range.to) {
      const error = new Error(t("error.invalidRange"));
      error.translationKey = "error.invalidRange";
      elements.customRangeError.textContent = t(error.translationKey);
      elements.customRangeError.hidden = false;
      return;
    }
  }
  if (preset !== "last-reset") state.customRange = range;
  state.rangePreset = preset === "last-reset" ? preset : "custom";
  state.rangeDraft = null;
  closeRangePicker(false);
  updateRangePickerTrigger();
  refreshAll({ reloadFilters: true, reason: "range" });
});
function changeResetIdentity() {
  const mode = state.lastResetDraft?.mode === "last" ? "last" : "current";
  state.lastResetDraft = { providerKey: elements.lastResetProvider.value,
    tierId: elements.lastResetTier.value, mode };
  renderLastResetEditor();
}
elements.lastResetProvider.addEventListener("change", () => {
  state.lastResetDraft = { providerKey: elements.lastResetProvider.value,
    mode: state.lastResetDraft?.mode === "last" ? "last" : "current" };
  renderLastResetEditor();
});
elements.lastResetTier.addEventListener("change", changeResetIdentity);
for (const [button, mode] of [[elements.resetCycleCurrent, "current"], [elements.resetCycleLast, "last"]]) {
  button.addEventListener("click", () => {
    state.lastResetDraft = { ...state.lastResetDraft, mode, cycleId: null, cycle: null };
    renderLastResetEditor();
  });
}
elements.resetCycleSelect.addEventListener("change", () => {
  state.lastResetDraft = { ...state.lastResetDraft, mode: "manual", cycleId: elements.resetCycleSelect.value };
  renderLastResetEditor();
});
elements.resetCycleRetry.addEventListener("click", () => resetCycleLoader.load());
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
  const activate = () => {
    state.activeRangeField = field.dataset.rangeField;
  };
  field.addEventListener("click", activate);
  field.addEventListener("focusin", activate);
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
    [$("historyReferenceDialog"), $("historyReferenceEdit"), closeHistoryDialog],
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
  select.addEventListener("change", () => refreshAll({ reason: "filters" }));
}

$("trendCompare").addEventListener("change", () => {
  const comparison = state.comparison;
  comparison.enabled = $("trendCompare").checked;
  chartInstances.trend?.clear();
  if (comparison.enabled) {
    comparison.savedMetric = elements.trendMetric.value;
    elements.trendMetric.value = "totalCostUsd";
    state.trendCumulative = true;
    comparison.selection = state.lastResetSelection ? { ...state.lastResetSelection } : null;
    comparison.estimatedQuota = true;
    comparison.predictDefaultPending = selectedRangeIncludesNow();
  } else {
    comparison.predictDefaultPending = null;
    historyQuotaLoader.cancel(); closeHistoryDialog();
    estimatedQuotaLoader.cancel();
    elements.trendMetric.value = comparison.savedMetric || "realTotalTokens";
  }
  renderTrend();
  if (state.chartBucket === "auto") {
    // Only Usage needs new aggregation when switching Auto mode. Reuse Quota.
    refreshOverview(undefined, state.quota?.range || state.overview?.range || selectedRange())
      .catch((error) => { if (error.name !== "AbortError") showError(error); });
  }
});
$("comparisonProvider").addEventListener("change", () => {
  state.comparison.selection = { providerKey: $("comparisonProvider").value, tierId: "" };
  renderTrend();
});
$("comparisonTier").addEventListener("change", () => {
  state.comparison.selection = { providerKey: $("comparisonProvider").value, tierId: $("comparisonTier").value };
  renderTrend();
});
$("comparisonPredict").addEventListener("change", () => {
  const key = comparisonPredictionId();
  if (!key) return;
  setQuotaPrediction(key, $("comparisonPredict").checked);
  renderQuota();
});
$("comparisonEstimatedQuota").addEventListener("change", () => {
  state.comparison.estimatedQuota = $("comparisonEstimatedQuota").checked;
  if (!state.comparison.estimatedQuota) estimatedQuotaLoader.cancel();
  renderTrend();
});
$("historyReferenceEdit").addEventListener("click", openHistoryDialog);
$("historyReferenceCancel").addEventListener("click", closeHistoryDialog);
$("historyReferenceDialog").addEventListener("cancel", (event) => { event.preventDefault(); closeHistoryDialog(); });
$("historyReferenceDialog").addEventListener("keydown", (event) => {
  if (event.key === "Escape") { event.preventDefault(); closeHistoryDialog(); }
});
$("historyReferenceSource").addEventListener("change", () => {
  historyDraft.mode = $("historyReferenceSource").value; renderHistoryDialog();
});
$("historyReferenceAmount").addEventListener("input", () => {
  historyDraft.amount = $("historyReferenceAmount").value; renderHistoryDialog();
});
$("historyReferenceCycle").addEventListener("change", () => {
  historyDraft.cycleId = $("historyReferenceCycle").value;
  historyDraft.identity = historyIdentity(historyContext()); renderHistoryDialog();
});
$("historyReferenceRetry").addEventListener("click", () => {
  historyQuotaService.invalidate(); historyPreviewLoader.cancel(); historyQuotaLoader.cancel();
  renderHistoryDialog(); renderTrend();
});
$("historyReferenceForm").addEventListener("submit", (event) => {
  event.preventDefault();
  const context = historyContext();
  if (!historyDraft || (historyDraft.mode !== "none"
    && (!context || !historyPreviewLoader.ensure(context, historyDraft).reference))) return;
  state.comparison.historyConfig = { ...historyDraft };
  state.comparison.historyEnabled = historyDraft.mode !== "none";
  closeHistoryDialog(); renderTrend();
});
elements.trendMetric.addEventListener("change", renderTrend);
elements.trendCumulative.addEventListener("change", () => {
  state.trendCumulative = elements.trendCumulative.checked;
  renderTrend();
});
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
for (const id of ["eventTierFilter", "eventEffortFilter"]) {
  $(id).addEventListener("change", () => {
    state.eventCursor = null; state.events = []; renderEventRows([], false); elements.loadMore.hidden = true;
    resource("events", (signal) => loadEvents({ signal }), () => {}).catch((error) => { if (error.name !== "AbortError") showError(error); });
  });
}
elements.loadMore.addEventListener("click", () => loadEvents({ append: true }).catch(showError));

document.addEventListener("visibilitychange", () => {
  if (!document.hidden) refreshAll();
});

updateRangePickerTrigger();
updateBucketPicker();
applyTheme();
applyTranslations();
if (typeof ResizeObserver === "function") {
  const chartResizeObserver = new ResizeObserver(() => {
    requestAnimationFrame(resizeCharts);
  });
  for (const element of [elements.trendChart, elements.quotaChart, elements.dailyHeatmap]) {
    chartResizeObserver.observe(element);
  }
}
window.addEventListener("resize", () => {
  resizeCharts();
});
initializeDashboard().catch(showError);
setInterval(() => {
  if (!document.hidden) refreshAll();
}, 30_000);
