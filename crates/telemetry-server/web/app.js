import { createFormatters, resolveLocale, translate } from "./i18n.js";

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
  rangePreset: $("rangePreset"),
  customRangeDialog: $("customRangeDialog"),
  customRangeForm: $("customRangeForm"),
  customRangeError: $("customRangeError"),
  cancelRange: $("cancelRange"),
  cancelRangeBottom: $("cancelRangeBottom"),
  customFrom: $("customFrom"),
  customTo: $("customTo"),
  applyRange: $("applyRange"),
  nodeFilter: $("nodeFilter"),
  appFilter: $("appFilter"),
  providerFilter: $("providerFilter"),
  modelFilter: $("modelFilter"),
  sourceFilter: $("sourceFilter"),
  trendMetric: $("trendMetric"),
  trendBucket: $("trendBucket"),
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
  lastNonCustomRange: "24h",
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
    }, formatters.dateTime.format(timestamp * 1000).replaceAll("/", "-")));
  });
}

function showTrendTooltip(point, event) {
  const tooltip = elements.trendTooltip;
  const panel = elements.trendChart.parentElement;
  tooltip.replaceChildren();
  const title = document.createElement("strong");
  title.textContent = formatters.dateTime.format(point.bucketStart * 1000);
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
    params.set("bucket", elements.trendBucket.value);
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

function openCustomRange() {
  elements.customRangeError.hidden = true;
  if (typeof elements.customRangeDialog.showModal === "function") {
    elements.customRangeDialog.showModal();
  } else {
    elements.customRangeDialog.setAttribute("open", "");
  }
  elements.customFrom.focus();
}

function closeCustomRange(restore = true) {
  if (restore) elements.rangePreset.value = state.lastNonCustomRange;
  if (typeof elements.customRangeDialog.close === "function") {
    elements.customRangeDialog.close();
  } else {
    elements.customRangeDialog.removeAttribute("open");
  }
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
  setConnection(state.connection.status, state.connection.key);
  elements.updatedAt.textContent = state.updatedAt
    ? t("status.updatedAt", { time: formatters.dateTime.format(state.updatedAt) })
    : t("status.neverUpdated");
  if (state.overview) renderOverview(state.overview);
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
elements.rangePreset.addEventListener("change", () => {
  if (elements.rangePreset.value === "custom") {
    openCustomRange();
    return;
  }
  state.lastNonCustomRange = elements.rangePreset.value;
  refreshAll({ reloadFilters: true });
});
elements.customRangeForm.addEventListener("submit", (event) => {
  event.preventDefault();
  try {
    selectedRange();
    closeCustomRange(false);
    refreshAll({ reloadFilters: true });
  } catch (error) {
    elements.customRangeError.textContent = error.translationKey ? t(error.translationKey) : error.message;
    elements.customRangeError.hidden = false;
  }
});
elements.cancelRange.addEventListener("click", () => closeCustomRange());
elements.cancelRangeBottom.addEventListener("click", () => closeCustomRange());
elements.customRangeDialog.addEventListener("cancel", (event) => {
  event.preventDefault();
  closeCustomRange();
});

for (const select of [elements.nodeFilter, elements.appFilter, elements.providerFilter, elements.modelFilter, elements.sourceFilter]) {
  select.addEventListener("change", () => refreshAll());
}

elements.trendMetric.addEventListener("change", renderTrend);
elements.trendBucket.addEventListener("change", () => refreshAll());
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
applyTheme();
applyTranslations();
refreshAll({ reloadFilters: true });
setInterval(() => {
  if (!document.hidden) refreshAll();
}, 30_000);
