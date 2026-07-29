import { createFormatters, resolveLocale, translate } from "./i18n.js";

const $ = (id) => document.getElementById(id);
const svgNs = "http://www.w3.org/2000/svg";
const localeStorageKey = "cc-switch-telemetry.locale";

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

const elements = {
  statusDot: $("statusDot"),
  statusText: $("statusText"),
  updatedAt: $("updatedAt"),
  languageToggle: $("languageToggle"),
  refreshButton: $("refreshButton"),
  errorBanner: $("errorBanner"),
  rangePreset: $("rangePreset"),
  customRange: $("customRange"),
  customFrom: $("customFrom"),
  customTo: $("customTo"),
  applyRange: $("applyRange"),
  nodeFilter: $("nodeFilter"),
  appFilter: $("appFilter"),
  providerFilter: $("providerFilter"),
  modelFilter: $("modelFilter"),
  sourceFilter: $("sourceFilter"),
  trendMetric: $("trendMetric"),
  trendChart: $("trendChart"),
  trendEmpty: $("trendEmpty"),
  coverageText: $("coverageText"),
  breakdownTabs: $("breakdownTabs"),
  breakdownRows: $("breakdownRows"),
  breakdownEmpty: $("breakdownEmpty"),
  eventRows: $("eventRows"),
  eventsEmpty: $("eventsEmpty"),
  loadMore: $("loadMore"),
};

const state = {
  overview: null,
  breakdownDimension: "nodes",
  eventCursor: null,
  requestController: null,
  eventsLoading: false,
  eventsGeneration: 0,
  events: [],
  lastError: null,
  updatedAt: null,
  connection: { status: "", key: "status.connecting" },
};

function epochNow() {
  return Math.floor(Date.now() / 1000);
}

function selectedRange() {
  const to = epochNow();
  switch (elements.rangePreset.value) {
    case "1h": return { from: to - 3600, to };
    case "7d": return { from: to - 7 * 86400, to };
    case "30d": return { from: to - 30 * 86400, to };
    case "custom": {
      const from = Math.floor(new Date(elements.customFrom.value).getTime() / 1000);
      const customTo = Math.floor(new Date(elements.customTo.value).getTime() / 1000);
      if (!Number.isFinite(from) || !Number.isFinite(customTo) || from >= customTo) {
        const error = new Error(t("error.invalidRange"));
        error.translationKey = "error.invalidRange";
        throw error;
      }
      return { from, to: customTo };
    }
    default: return { from: to - 86400, to };
  }
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
  for (const value of values) {
    if (!value) continue;
    const option = document.createElement("option");
    option.value = value;
    option.textContent = value;
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

function renderSummary(summary) {
  $("kpiRequests").textContent = formatters.integerNumber.format(summary.totalRequests);
  $("kpiSuccessCount").textContent = t("kpi.successCount", {
    count: formatters.integerNumber.format(summary.successfulRequests),
  });
  $("kpiTokens").textContent = formatTokens(summary.realTotalTokens);
  $("kpiTokenParts").textContent = t("kpi.tokenParts", {
    input: formatTokens(summary.freshInputTokens),
    output: formatTokens(summary.outputTokens),
  });
  $("kpiCost").textContent = formatters.moneyNumber.format(summary.totalCostUsd);
  $("kpiSuccessRate").textContent = formatPercent(summary.successRate);
  $("kpiCacheRate").textContent = formatPercent(summary.cacheHitRate, true);
  $("kpiCacheTokens").textContent = t("kpi.cacheTokens", {
    count: formatTokens(summary.cacheReadTokens),
  });
  $("kpiLatency").textContent = formatLatency(summary.avgLatencyMs);
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

function renderTrend() {
  const points = state.overview?.trend || [];
  elements.trendChart.replaceChildren();
  elements.trendEmpty.hidden = points.length > 0;
  elements.trendChart.hidden = points.length === 0;
  if (!points.length) return;

  const metric = elements.trendMetric.value;
  const width = 900;
  const height = 280;
  const padding = { left: 64, right: 22, top: 20, bottom: 42 };
  const plotWidth = width - padding.left - padding.right;
  const plotHeight = height - padding.top - padding.bottom;
  const values = points.map((point) => trendValue(point, metric));
  const maximum = Math.max(...values, 1);
  const x = (index) => padding.left + (points.length === 1 ? plotWidth / 2 : index * plotWidth / (points.length - 1));
  const y = (value) => padding.top + plotHeight - value / maximum * plotHeight;

  for (let index = 0; index <= 4; index += 1) {
    const gridY = padding.top + index * plotHeight / 4;
    elements.trendChart.append(createSvg("line", { x1: padding.left, x2: width - padding.right, y1: gridY, y2: gridY, class: "grid" }));
    const labelValue = maximum * (1 - index / 4);
    elements.trendChart.append(createSvg("text", { x: padding.left - 10, y: gridY + 4, "text-anchor": "end", class: "axis-label" }, trendValueLabel(labelValue, metric)));
  }

  const coordinates = points.map((point, index) => [x(index), y(values[index])]);
  const linePath = coordinates.map(([cx, cy], index) => `${index ? "L" : "M"} ${cx} ${cy}`).join(" ");
  const areaPath = `${linePath} L ${coordinates.at(-1)[0]} ${padding.top + plotHeight} L ${coordinates[0][0]} ${padding.top + plotHeight} Z`;
  elements.trendChart.append(createSvg("path", { d: areaPath, class: "area" }));
  elements.trendChart.append(createSvg("path", { d: linePath, class: "line" }));

  const labelIndexes = new Set([0, Math.floor((points.length - 1) / 3), Math.floor(2 * (points.length - 1) / 3), points.length - 1]);
  points.forEach((point, index) => {
    const [cx, cy] = coordinates[index];
    const circle = createSvg("circle", { cx, cy, r: 4, class: "point" });
    circle.append(createSvg("title", {}, `${formatters.dateTime.format(point.bucketStart * 1000)} · ${trendValueLabel(values[index], metric)}`));
    elements.trendChart.append(circle);
    if (labelIndexes.has(index)) {
      elements.trendChart.append(createSvg("text", { x: cx, y: height - 13, "text-anchor": index === 0 ? "start" : index === points.length - 1 ? "end" : "middle", class: "axis-label" }, formatters.dateTime.format(point.bucketStart * 1000).replaceAll("/", "-")));
    }
  });
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
    appendCell(row, item.key || t("common.unknown"));
    appendCell(row, formatters.integerNumber.format(item.totalRequests));
    appendCell(row, formatTokens(item.realTotalTokens));
    appendCell(row, formatPercent(item.successRate));
    appendCell(row, formatters.moneyNumber.format(item.totalCostUsd));
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
  renderTrend();
  renderBreakdown();
  renderCoverage(overview.coverage);
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
    appendCell(row, `${item.providerId || "—"} / ${item.model || item.requestModel || "—"}`);
    appendCell(row, formatTokens(item.realTotalTokens));
    appendCell(row, formatTokens(item.cacheReadTokens));
    appendCell(row, formatters.moneyNumber.format(item.totalCostUsd));
    appendCell(row, formatLatency(item.latencyMs));
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
    params.set("bucket", "auto");
    const [overview] = await Promise.all([
      fetchJson(`/v1/dashboard/overview?${params}`, controller.signal),
      loadEvents({ append: false, signal: controller.signal }),
    ]);
    renderOverview(overview);
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

function toLocalInputValue(timestamp) {
  const date = new Date(timestamp);
  const offset = date.getTimezoneOffset() * 60_000;
  return new Date(timestamp - offset).toISOString().slice(0, 16);
}

function initializeCustomRange() {
  const now = Date.now();
  elements.customFrom.value = toLocalInputValue(now - 24 * 60 * 60 * 1000);
  elements.customTo.value = toLocalInputValue(now);
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
  updateFilterPlaceholders();
  setConnection(state.connection.status, state.connection.key);
  elements.updatedAt.textContent = state.updatedAt
    ? t("status.updatedAt", { time: formatters.dateTime.format(state.updatedAt) })
    : t("status.neverUpdated");
  if (state.overview) renderOverview(state.overview);
  renderEventRows(state.events, false);
  if (state.lastError) showError(state.lastError);
}

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
elements.applyRange.addEventListener("click", () => refreshAll({ reloadFilters: true }));
elements.rangePreset.addEventListener("change", () => {
  elements.customRange.hidden = elements.rangePreset.value !== "custom";
  if (elements.rangePreset.value !== "custom") refreshAll({ reloadFilters: true });
});

for (const select of [elements.nodeFilter, elements.appFilter, elements.providerFilter, elements.modelFilter, elements.sourceFilter]) {
  select.addEventListener("change", () => refreshAll());
}

elements.trendMetric.addEventListener("change", renderTrend);
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

initializeCustomRange();
applyTranslations();
refreshAll({ reloadFilters: true });
setInterval(() => {
  if (!document.hidden) refreshAll();
}, 30_000);
