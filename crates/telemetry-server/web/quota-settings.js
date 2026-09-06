export function providerIdentity(provider) {
  return JSON.stringify([provider.nodeId, provider.providerId]);
}
export function metricIdentity(metric) {
  return JSON.stringify([metric.key || "", metric.kind || "", metric.unit || ""]);
}
export function providerName(provider, settings) {
  return settings?.quotaProviderAliases?.find((entry) => providerIdentity(entry) === providerIdentity(provider))?.alias
    || provider.providerName || provider.providerId;
}
export function providerSelected(selection, provider) {
  return selection == null || selection.some((entry) => providerIdentity(entry) === providerIdentity(provider));
}
export function metricSelected(selection, provider, metric) {
  if (selection == null) return true;
  const entry = selection.find((entry) => providerIdentity(entry) === providerIdentity(provider));
  return Boolean(entry && (entry.metrics === null || entry.metrics.some((item) => metricIdentity(item) === metricIdentity(metric))));
}
export function mergeProviders(providers, settings, selection = undefined) {
  const result = new Map();
  for (const provider of providers || []) {
    const metrics = new Map();
    for (const metric of [...(provider.metrics || []), ...(provider.current || []), ...(provider.series || [])]) {
      metrics.set(metricIdentity(metric), { key: metric.key, kind: metric.kind, unit: metric.unit || null, label: metric.label });
    }
    result.set(providerIdentity(provider), { ...provider, metrics: [...metrics.values()] });
  }
  for (const entry of [...(settings?.quotaDefaults?.providers || []), ...(settings?.quotaProviderAliases || []), ...(selection || [])]) {
    const id = providerIdentity(entry);
    if (!result.has(id)) result.set(id, { nodeId: entry.nodeId, providerId: entry.providerId, unavailable: true, metrics: [] });
    const target = result.get(id);
    for (const metric of entry.metrics || []) {
      if (!target.metrics.some((m) => metricIdentity(m) === metricIdentity(metric))) {
        target.metrics.push({ ...metric, label: metric.key, unavailable: true });
      }
    }
  }
  return [...result.values()];
}

// Native popovers escape the clipped chart panels and support click-away/Escape.
export function renderPicker(host, options) {
  const { title, groups, selected, onChange, allLabel, noneLabel } = options;
  const wasOpen = Boolean(host.querySelector(":popover-open"));
  const focusValue = host.ownerDocument.activeElement?.dataset?.value;
  host.replaceChildren();
  const trigger = document.createElement("button");
  trigger.type = "button";
  trigger.className = "quota-picker-trigger";
  trigger.textContent = `${title}: ${selected === null ? allLabel : selected.length}`;
  trigger.setAttribute("aria-label", title);
  trigger.setAttribute("aria-expanded", "false");
  const menu = document.createElement("div");
  menu.className = "quota-picker-menu";
  menu.setAttribute("popover", "auto");
  menu.setAttribute("aria-label", title);
  const position = () => {
    const rect = trigger.getBoundingClientRect();
    const width = document.documentElement.clientWidth;
    const height = document.documentElement.clientHeight;
    const box = menu.getBoundingClientRect();
    menu.style.left = `${Math.max(12, Math.min(rect.left, width - box.width - 12))}px`;
    menu.style.top = `${Math.max(12, Math.min(rect.bottom + 6, height - box.height - 12))}px`;
  };
  menu.addEventListener("toggle", () => {
    const open = menu.matches(":popover-open");
    trigger.setAttribute("aria-expanded", String(open));
    if (open) position();
  });
  trigger.addEventListener("click", () => { menu.togglePopover(); position(); });
  const update = (next) => {
    renderPicker(host, { ...options, selected: next });
    onChange(next);
  };
  const actions = document.createElement("div");
  actions.className = "quota-picker-actions";
  for (const [label, value] of [[allLabel, null], [noneLabel, []]]) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = label;
    button.addEventListener("click", () => update(value));
    actions.append(button);
  }
  menu.append(actions);
  const values = groups.flatMap((group) => group.options.map((item) => item.value));
  for (const group of groups) {
    const fieldset = document.createElement("fieldset");
    if (group.label) {
      const legend = document.createElement("legend");
      legend.textContent = group.label;
      fieldset.append(legend);
    }
    for (const item of group.options) {
      const label = document.createElement("label");
      const input = document.createElement("input");
      input.type = "checkbox";
      input.dataset.value = item.value;
      input.checked = selected === null || selected.includes(item.value);
      input.addEventListener("change", () => {
        const next = new Set(selected === null ? values : selected);
        if (input.checked) next.add(item.value); else next.delete(item.value);
        update([...next]);
      });
      label.append(input, item.label);
      fieldset.append(label);
    }
    menu.append(fieldset);
  }
  host.append(trigger, menu);
  if (wasOpen) {
    menu.showPopover();
    position();
    const focus = [...menu.querySelectorAll("input")].find((input) => input.dataset.value === focusValue);
    (focus || trigger).focus({ preventScroll: true });
  }
}
