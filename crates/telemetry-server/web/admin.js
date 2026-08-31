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
};

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
    await loadNodes();
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

async function logout() {
  await api("/admin/logout", { method: "POST" }).catch(() => {});
  showLogin();
}

async function boot() {
  try {
    await api("/admin/api/session");
    showAdmin();
    await loadNodes();
  } catch (error) {
    if (error.status === 503) showLogin("管理员功能未启用：请设置 ADMIN_PASSWORD 后重启 server。");
    else showLogin();
  }
}

elements.loginForm.addEventListener("submit", login);
elements.createForm.addEventListener("submit", createNode);
elements.logoutButton.addEventListener("click", logout);
elements.closeTokenButton.addEventListener("click", () => { elements.tokenPanel.hidden = true; });
elements.copyTokenButton.addEventListener("click", async () => {
  await navigator.clipboard.writeText(elements.tokenValue.textContent);
  showMessage(elements.actionMessage, "Token 已复制。", "ok");
});
boot();
