import {
  providerIdentity,
  metricIdentity,
  providerSelected,
  mergeProviders,
  renderPicker,
  quotaTierPeriodLabel,
  quotaTierPeriodSeconds,
} from "./quota-settings.js";
const elements = {
  loginPanel: document.getElementById("loginPanel"),
  loginForm: document.getElementById("loginForm"),
  passwordInput: document.getElementById("passwordInput"),
  loginMessage: document.getElementById("loginMessage"),
  adminPanel: document.getElementById("adminPanel"),
  logoutButton: document.getElementById("logoutButton"),
  createForm: document.getElementById("createForm"),
  nodeNameInput: document.getElementById("nodeNameInput"),
  actionMessage: document.getElementById("actionMessage"),
  nodeRows: document.getElementById("nodeRows"),
  emptyMessage: document.getElementById("emptyMessage"),
  tokenPanel: document.getElementById("tokenPanel"),
  tokenValue: document.getElementById("tokenValue"),
  closeTokenButton: document.getElementById("closeTokenButton"),
  copyTokenButton: document.getElementById("copyTokenButton"),
  logPreviewForm: document.getElementById("logPreviewForm"),
  logKindInput: document.getElementById("logKindInput"),
  logNodeInput: document.getElementById("logNodeInput"),
  logMessage: document.getElementById("logMessage"),
  logPreviewPanel: document.getElementById("logPreviewPanel"),
  logWarning: document.getElementById("logWarning"),
  logCountList: document.getElementById("logCountList"),
  logConfirmationValue: document.getElementById("logConfirmationValue"),
  logConfirmationInput: document.getElementById("logConfirmationInput"),
  purgeLogsButton: document.getElementById("purgeLogsButton"),
};

let lastLogPreview = null;

async function api(url, options = {}) {
  const headers = { Accept: "application/json", ...(options.headers || {}) };
  if (options.body && !headers["Content-Type"]) headers["Content-Type"] = "application/json";
  const response = await fetch(url, { ...options, headers, credentials: "same-origin" });
  let body = null;
  try { body = await response.json(); } catch { /* keep status */ }
  if (!response.ok) {
    const error = new Error(body?.message || `${response.status} ${response.statusText}`);
    error.status = response.status;
    throw error;
  }
  return body;
}

function showMessage(element, message, kind = "") {
  element.textContent = message || "";
  element.style.color = kind === "ok" ? "#91e4b4" : "";
}

function showLogin(message = "") {
  elements.loginPanel.hidden = false;
  elements.adminPanel.hidden = true;
  elements.logoutButton.hidden = true;
  showMessage(elements.loginMessage, message);
}

function showAdmin() {
  elements.loginPanel.hidden = true;
  elements.adminPanel.hidden = false;
  elements.logoutButton.hidden = false;
}

function showToken(token) {
  elements.tokenValue.textContent = token;
  elements.tokenPanel.hidden = false;
  elements.tokenPanel.scrollIntoView({ behavior: "smooth", block: "center" });
}

function appendTextCell(row, value) {
  const cell = document.createElement("td");
  cell.textContent = value || "—";
  row.append(cell);
}

function renderNodes(nodes) {
  elements.nodeRows.replaceChildren();
  elements.emptyMessage.hidden = nodes.length > 0;
  for (const node of nodes) {
    const row = document.createElement("tr");
    appendTextCell(row, node.nodeName);
    const uuid = document.createElement("td");
    const uuidCode = document.createElement("code");
    uuidCode.textContent = node.uuid;
    uuid.append(uuidCode);
    row.append(uuid);
    appendTextCell(row, node.hasToken ? "有效" : "已撤销");
    appendTextCell(row, new Date(node.updatedAt * 1000).toLocaleString());
    const actions = document.createElement("td");
    const group = document.createElement("div");
    group.className = "action-group";
    const rename = document.createElement("button");
    rename.className = "button secondary";
    rename.textContent = "改名";
    rename.addEventListener("click", () => renameNode(node));
    const token = document.createElement("button");
    token.className = "button secondary";
    token.textContent = node.hasToken ? "重新生成 Token" : "生成 Token";
    token.addEventListener("click", () => regenerateToken(node));
    const revoke = document.createElement("button");
    revoke.className = "button danger";
    revoke.textContent = "撤销 Token";
    revoke.disabled = !node.hasToken;
    revoke.addEventListener("click", () => revokeToken(node));
    group.append(rename, token, revoke);
    actions.append(group);
    row.append(actions);
    elements.nodeRows.append(row);
  }
  const selectedNode = elements.logNodeInput.value;
  elements.logNodeInput.replaceChildren(new Option("全部节点", ""));
  for (const node of nodes) {
    elements.logNodeInput.append(new Option(`${node.nodeName} (${node.uuid})`, node.uuid));
  }
  elements.logNodeInput.value = nodes.some((node) => node.uuid === selectedNode) ? selectedNode : "";
}

function resetLogPreview() {
  lastLogPreview = null;
  elements.logPreviewPanel.hidden = true;
  elements.logConfirmationInput.value = "";
  elements.purgeLogsButton.disabled = true;
}

function renderLogPreview(preview) {
  lastLogPreview = preview;
  elements.logWarning.textContent = preview.warning;
  elements.logCountList.replaceChildren();
  for (const [table, count] of Object.entries(preview.counts)) {
    const item = document.createElement("li");
    const name = document.createElement("code");
    name.textContent = table;
    item.append(name, `: ${count}`);
    elements.logCountList.append(item);
  }
  elements.logConfirmationValue.textContent = preview.confirmation;
  elements.logConfirmationInput.value = "";
  elements.purgeLogsButton.disabled = true;
  elements.logPreviewPanel.hidden = false;
}

async function loadNodes() {
  try {
    renderNodes(await api("/admin/api/nodes"));
  } catch (error) {
    if (error.status === 401) return showLogin("登录已过期，请重新登录。");
    showMessage(elements.actionMessage, error.message);
  }
}

async function login(event) {
  event.preventDefault();
  showMessage(elements.loginMessage, "登录中…");
  try {
    await api("/admin/login", {
      method: "POST",
      body: JSON.stringify({ password: elements.passwordInput.value }),
    });
    elements.passwordInput.value = "";
    showAdmin();
    await Promise.all([loadNodes(), loadSettings()]);
  } catch (error) {
    showMessage(elements.loginMessage, error.message);
  }
}

async function createNode(event) {
  event.preventDefault();
  try {
    const result = await api("/admin/api/nodes", {
      method: "POST",
      body: JSON.stringify({ nodeName: elements.nodeNameInput.value }),
    });
    elements.nodeNameInput.value = "";
    showToken(result.token);
    showMessage(elements.actionMessage, "节点已创建，请立即保存 Token。", "ok");
    await loadNodes();
  } catch (error) {
    showMessage(elements.actionMessage, error.message);
  }
}

async function renameNode(node) {
  const name = window.prompt("新的 Node_Name", node.nodeName);
  if (name === null) return;
  try {
    await api(`/admin/api/nodes/${encodeURIComponent(node.uuid)}`, {
      method: "PATCH",
      body: JSON.stringify({ nodeName: name }),
    });
    showMessage(elements.actionMessage, "Node_Name 已更新。", "ok");
    await loadNodes();
  } catch (error) {
    showMessage(elements.actionMessage, error.message);
  }
}

async function regenerateToken(node) {
  if (!window.confirm(`为 ${node.nodeName} 重新生成 Token？旧 Token 会立即失效。`)) return;
  try {
    const result = await api(`/admin/api/nodes/${encodeURIComponent(node.uuid)}/token`, { method: "POST" });
    showToken(result.token);
    showMessage(elements.actionMessage, "Token 已重新生成，请立即保存。", "ok");
    await loadNodes();
  } catch (error) {
    showMessage(elements.actionMessage, error.message);
  }
}

async function revokeToken(node) {
  if (!window.confirm(`撤销 ${node.nodeName} 的 Token？该节点将无法继续上传。`)) return;
  try {
    await api(`/admin/api/nodes/${encodeURIComponent(node.uuid)}/token`, { method: "DELETE" });
    showMessage(elements.actionMessage, "Token 已撤销。", "ok");
    await loadNodes();
  } catch (error) {
    showMessage(elements.actionMessage, error.message);
  }
}

async function previewLogs(event) {
  event.preventDefault();
  resetLogPreview();
  const parameters = new URLSearchParams({ kind: elements.logKindInput.value });
  if (elements.logNodeInput.value) parameters.set("node_id", elements.logNodeInput.value);
  try {
    const preview = await api(`/admin/api/logs/preview?${parameters}`);
    renderLogPreview(preview);
    showMessage(elements.logMessage, `将删除 ${preview.totalRows} 行（含缓存与同步状态）。`, "ok");
  } catch (error) {
    if (error.status === 401) return showLogin("登录已过期，请重新登录。");
    showMessage(elements.logMessage, error.message);
  }
}

async function purgeLogs() {
  if (!lastLogPreview || elements.logConfirmationInput.value !== lastLogPreview.confirmation) return;
  const scope = lastLogPreview.nodeId || "全部节点";
  if (!window.confirm(`永久删除 ${scope} 的${lastLogPreview.kind === "request" ? "请求" : "Quota"}日志？`)) return;
  try {
    const result = await api("/admin/api/logs/purge", {
      method: "POST",
      body: JSON.stringify({
        kind: lastLogPreview.kind,
        nodeId: lastLogPreview.nodeId,
        confirmation: elements.logConfirmationInput.value,
      }),
    });
    resetLogPreview();
    showMessage(elements.logMessage, `已删除 ${result.totalRows} 行。${result.warning}`, "ok");
  } catch (error) {
    if (error.status === 401) return showLogin("登录已过期，请重新登录。");
    showMessage(elements.logMessage, error.message);
  }
}

async function logout() {
  await api("/admin/logout", { method: "POST" }).catch(() => {});
  showLogin();
}

async function boot() {
  try {
    await api("/admin/api/session");
    showAdmin();
    await Promise.all([loadNodes(), loadSettings()]);
  } catch (error) {
    if (error.status === 503) showLogin("管理员功能未启用：请设置 ADMIN_PASSWORD 后重启 server。");
    else showLogin();
  }
}

elements.loginForm.addEventListener("submit", login);
elements.createForm.addEventListener("submit", createNode);
elements.logPreviewForm.addEventListener("submit", previewLogs);
elements.logKindInput.addEventListener("change", resetLogPreview);
elements.logNodeInput.addEventListener("change", resetLogPreview);
elements.logConfirmationInput.addEventListener("input", () => {
  elements.purgeLogsButton.disabled = !lastLogPreview
    || elements.logConfirmationInput.value !== lastLogPreview.confirmation;
});
elements.purgeLogsButton.addEventListener("click", purgeLogs);
elements.logoutButton.addEventListener("click", logout);
elements.closeTokenButton.addEventListener("click", () => { elements.tokenPanel.hidden = true; });
elements.copyTokenButton.addEventListener("click", async () => {
  await navigator.clipboard.writeText(elements.tokenValue.textContent);
  showMessage(elements.actionMessage, "Token 已复制。", "ok");
});
boot();


let settingsDraft = null;
let settingsCatalog = [];
const settingsForm = document.getElementById("settingsForm");
const settingsMessage = document.getElementById("settingsMessage");
const allProvidersInput = document.getElementById("settingsAllProviders");
const settingsRangePreset = document.getElementById("settingsRangePreset");
const settingsTimeFormat = document.getElementById("settingsTimeFormat");
const settingsModelBillingMultipliers = document.getElementById("settingsModelBillingMultipliers");
const addModelBillingMultiplier = document.getElementById("addModelBillingMultiplier");
const settingsResetProvider = document.getElementById("settingsResetProvider");
const settingsResetTier = document.getElementById("settingsResetTier");
const settingsResetMessage = document.getElementById("settingsResetMessage");

function ensureDashboardDefaults() {
  settingsDraft.dashboardDefaults = {
    rangePreset: "24h",
    timeFormat: "24h",
    modelBillingMultipliers: [],
    lastReset: null,
    ...(settingsDraft.dashboardDefaults || {}),
  };
  if (!Array.isArray(settingsDraft.dashboardDefaults.modelBillingMultipliers)) {
    settingsDraft.dashboardDefaults.modelBillingMultipliers = [];
  }
}

function resetDefaultGroups() {
  return settingsCatalog.map((provider) => ({
    provider,
    tiers: (provider.metrics || [])
      .filter((metric) => metric.kind === "utilizationPercent")
      .map((metric) => ({
        metric,
        periodSeconds: quotaTierPeriodSeconds(metric),
      }))
      .filter(({ periodSeconds }) => Number.isSafeInteger(periodSeconds) && periodSeconds > 0)
      .map((tier) => ({
        ...tier,
        id: metricIdentity(tier.metric),
        periodLabel: quotaTierPeriodLabel(tier.periodSeconds),
      })),
  })).filter(({ tiers }) => tiers.length > 0);
}

function setDashboardResetSelection(group, tier) {
  settingsDraft.dashboardDefaults.lastReset = group && tier
    ? {
      nodeId: group.provider.nodeId,
      providerId: group.provider.providerId,
      metricKey: tier.metric.key,
      metricKind: tier.metric.kind,
      unit: tier.metric.unit || null,
    }
    : null;
}

function renderModelBillingMultipliers() {
  const entries = settingsDraft.dashboardDefaults.modelBillingMultipliers;
  if (!entries.length) entries.push({ model: "", multiplier: 1 });
  settingsModelBillingMultipliers.replaceChildren();
  for (const entry of entries) {
    const row = document.createElement("div");
    row.className = "settings-model-billing-row";
    const modelLabel = document.createElement("label");
    modelLabel.className = "settings-model-billing-model";
    modelLabel.textContent = "模型名称";
    const modelInput = document.createElement("input");
    modelInput.className = "settings-model-billing-model-input";
    modelInput.type = "text";
    modelInput.maxLength = 256;
    modelInput.placeholder = "例如 gpt-5";
    modelInput.value = entry.model || "";
    modelInput.addEventListener("input", () => {
      entry.model = modelInput.value;
      settingsChanged();
    });
    modelLabel.append(modelInput);

    const multiplierLabel = document.createElement("label");
    multiplierLabel.className = "settings-model-billing-multiplier";
    multiplierLabel.textContent = "倍率";
    const multiplierInput = document.createElement("input");
    multiplierInput.className = "settings-model-billing-multiplier-input";
    multiplierInput.type = "number";
    multiplierInput.min = "0";
    multiplierInput.max = "1000";
    multiplierInput.step = "0.01";
    multiplierInput.inputMode = "decimal";
    multiplierInput.value = String(entry.multiplier ?? 1);
    multiplierInput.addEventListener("input", () => {
      const value = Number(multiplierInput.value);
      if (Number.isFinite(value) && value >= 0 && value <= 1000) entry.multiplier = value;
      settingsChanged();
    });
    multiplierLabel.append(multiplierInput);

    const remove = document.createElement("button");
    remove.className = "button danger";
    remove.type = "button";
    remove.textContent = "删除";
    remove.addEventListener("click", () => {
      const index = entries.indexOf(entry);
      if (index >= 0) entries.splice(index, 1);
      renderModelBillingMultipliers();
      settingsChanged();
    });
    row.append(modelLabel, multiplierLabel, remove);
    settingsModelBillingMultipliers.append(row);
  }
}

function renderDashboardDefaults() {
  ensureDashboardDefaults();
  settingsRangePreset.value = settingsDraft.dashboardDefaults.rangePreset;
  settingsTimeFormat.value = settingsDraft.dashboardDefaults.timeFormat;
  renderModelBillingMultipliers();
  const groups = resetDefaultGroups();
  const configured = settingsDraft.dashboardDefaults.lastReset;
  const configuredProvider = configured ? providerIdentity(configured) : null;
  const group = groups.find(({ provider }) => providerIdentity(provider) === configuredProvider) || groups[0];
  const configuredMetric = configured ? metricIdentity(configured) : null;
  const tier = group?.tiers.find(({ metric }) => metricIdentity(metric) === configuredMetric) || group?.tiers[0];
  settingsResetProvider.replaceChildren();
  for (const item of groups) {
    const option = new Option(
      `${item.provider.nodeName || item.provider.nodeId} / ${item.provider.providerName || item.provider.providerId}`,
      providerIdentity(item.provider),
    );
    settingsResetProvider.append(option);
  }
  settingsResetProvider.disabled = groups.length === 0;
  if (group) settingsResetProvider.value = providerIdentity(group.provider);
  settingsResetTier.replaceChildren();
  for (const item of group?.tiers || []) {
    settingsResetTier.append(new Option(
      `${item.periodLabel} · ${item.metric.label || item.metric.key}`,
      item.id,
    ));
  }
  settingsResetTier.disabled = !tier;
  if (tier) settingsResetTier.value = tier.id;
  settingsResetMessage.textContent = tier
    ? "选择“最近一次重置”后使用这里保存的 Provider 和 tier。"
    : "当前没有可识别周期的 Quota reset tier；保存后 Dashboard 会在有可用数据时回退到首个 tier。";
}

function customProviders() {
  if (settingsDraft.quotaDefaults.providers === null) {
    settingsDraft.quotaDefaults.providers = settingsCatalog.map(({ nodeId, providerId }) => ({ nodeId, providerId, metrics: null }));
  }
  return settingsDraft.quotaDefaults.providers;
}
function settingsChanged() { showMessage(settingsMessage, "有未保存的修改。"); }
function renderSettings() {
  renderDashboardDefaults();
  allProvidersInput.checked = settingsDraft.quotaDefaults.providers === null;
  const container = document.getElementById("settingsProviders");
  container.replaceChildren();
  for (const provider of settingsCatalog) {
    const identity = providerIdentity(provider);
    const row = document.createElement("div");
    row.className = "settings-provider";
    const label = document.createElement("label");
    label.className = "check-label";
    const enabled = document.createElement("input");
    enabled.type = "checkbox";
    enabled.checked = providerSelected(settingsDraft.quotaDefaults.providers, provider);
    label.append(enabled, `${provider.nodeName || provider.nodeId} / ${provider.providerName || provider.providerId}${provider.unavailable ? '（暂无数据）' : ''}`);
    label.title = provider.nodeId + ' / ' + provider.providerId;
    enabled.addEventListener("change", () => {
      const entries = customProviders().filter((p) => providerIdentity(p) !== identity);
      if (enabled.checked) entries.push({ nodeId: provider.nodeId, providerId: provider.providerId, metrics: null });
      settingsDraft.quotaDefaults.providers = entries;
      settingsChanged(); renderSettings();
    });
    const aliasLabel = document.createElement("label");
    aliasLabel.textContent = "显示别名";
    const alias = document.createElement("input");
    alias.maxLength = 256;
    alias.placeholder = provider.providerName || provider.providerId;
    alias.value = settingsDraft.quotaProviderAliases.find((p) => providerIdentity(p) === identity)?.alias || "";
    alias.addEventListener("input", () => {
      settingsDraft.quotaProviderAliases = settingsDraft.quotaProviderAliases.filter((p) => providerIdentity(p) !== identity);
      if (alias.value.trim()) settingsDraft.quotaProviderAliases.push({ nodeId: provider.nodeId, providerId: provider.providerId, alias: alias.value });
      settingsChanged();
    });
    aliasLabel.append(alias);
    const metricsLabel = document.createElement("label");
    metricsLabel.className = "settings-provider-metrics";
    metricsLabel.textContent = "默认指标";
    const picker = document.createElement("div");
    picker.className = "quota-picker";
    const metrics = settingsDraft.quotaDefaults.providers?.find((p) => providerIdentity(p) === identity)?.metrics ?? null;
    renderPicker(picker, {
      title: "默认指标", allLabel: "全部", noneLabel: "清空",
      showTitleInTrigger: false,
      selected: metrics === null ? null : metrics.map(metricIdentity),
      groups: [{ options: provider.metrics.map((m) => ({ value: metricIdentity(m), label: `${m.label || m.key} · ${m.unit || m.kind}` })) }],
      onChange: (keys) => {
        const entries = customProviders();
        let entry = entries.find((p) => providerIdentity(p) === identity);
        if (!entry) { entry = { nodeId: provider.nodeId, providerId: provider.providerId, metrics: null }; entries.push(entry); }
        entry.metrics = keys === null ? null : keys.map((key) => { const [keyName, kind, unit] = JSON.parse(key); return { key: keyName, kind, unit: unit || null }; });
        allProvidersInput.checked = false;
        enabled.checked = true;
        settingsChanged();
      },
    });
    metricsLabel.append(picker);
    row.append(label, aliasLabel, metricsLabel); container.append(row);
  }
}
async function loadSettings() {
  document.getElementById("saveSettings").disabled = true;
  try {
    const result = await api("/admin/api/settings");
    settingsDraft = result.settings;
    settingsCatalog = mergeProviders(result.providers, settingsDraft);
    ensureDashboardDefaults();
    renderSettings();
    document.getElementById("saveSettings").disabled = false;
  } catch (error) {
    if (error.status === 401) return showLogin("登录已过期，请重新登录。");
    showMessage(settingsMessage, error.message);
  }
}
allProvidersInput.addEventListener("change", () => {
  if (!settingsDraft) return;
  if (allProvidersInput.checked) settingsDraft.quotaDefaults.providers = null;
  else customProviders();
  settingsChanged(); renderSettings();
});
settingsRangePreset.addEventListener("change", () => {
  if (!settingsDraft) return;
  settingsDraft.dashboardDefaults.rangePreset = settingsRangePreset.value;
  settingsChanged();
});
settingsTimeFormat.addEventListener("change", () => {
  if (!settingsDraft) return;
  settingsDraft.dashboardDefaults.timeFormat = settingsTimeFormat.value;
  settingsChanged();
});
addModelBillingMultiplier.addEventListener("click", () => {
  if (!settingsDraft) return;
  settingsDraft.dashboardDefaults.modelBillingMultipliers.push({ model: "", multiplier: 1 });
  renderModelBillingMultipliers();
  settingsChanged();
});
settingsResetProvider.addEventListener("change", () => {
  if (!settingsDraft) return;
  const group = resetDefaultGroups().find(({ provider }) => providerIdentity(provider) === settingsResetProvider.value);
  setDashboardResetSelection(group, group?.tiers[0]);
  renderDashboardDefaults();
  settingsChanged();
});
settingsResetTier.addEventListener("change", () => {
  if (!settingsDraft) return;
  const group = resetDefaultGroups().find(({ provider }) => providerIdentity(provider) === settingsResetProvider.value);
  const tier = group?.tiers.find((item) => item.id === settingsResetTier.value);
  setDashboardResetSelection(group, tier);
  settingsChanged();
});
settingsForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!settingsDraft) return;
  const button = document.getElementById("saveSettings");
  button.disabled = true;
  const entries = [...settingsModelBillingMultipliers.querySelectorAll(".settings-model-billing-row")];
  const seen = new Set();
  const multipliers = [];
  for (const row of entries) {
    const model = row.querySelector('input[type="text"]').value.trim();
    const multiplier = Number(row.querySelector('input[type="number"]').value);
    if (!model && multiplier === 1) continue;
    const hasControl = [...model].some((character) => /\p{C}/u.test(character));
    if (!model || new TextEncoder().encode(model).length > 256 || hasControl || seen.has(model)) {
      showMessage(settingsMessage, "模型名称必须非空、唯一且不超过 256 字节。");
      button.disabled = false;
      return;
    }
    if (!Number.isFinite(multiplier) || multiplier < 0 || multiplier > 1000) {
      showMessage(settingsMessage, "模型计费倍率必须是 0 到 1000 之间的数字。");
      button.disabled = false;
      return;
    }
    seen.add(model);
    multipliers.push({ model, multiplier });
  }
  settingsDraft.dashboardDefaults.modelBillingMultipliers = multipliers;
  try {
    settingsDraft = await api("/admin/api/settings", { method: "PUT", body: JSON.stringify(settingsDraft) });
    renderSettings();
    showMessage(settingsMessage, "设置已保存。Dashboard 下次打开时应用。", "ok");
  } catch (error) {
    if (error.status === 401) showLogin("登录已过期，请重新登录。");
    else showMessage(settingsMessage, error.message);
  } finally { button.disabled = false; }
});
