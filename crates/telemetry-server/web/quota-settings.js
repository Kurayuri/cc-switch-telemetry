const RESET_PERIOD_LIMIT_SECONDS = 720 * 24 * 60 * 60;
const PERIOD_UNITS = [
  ["mo", 30 * 24 * 60 * 60],
  ["w", 7 * 24 * 60 * 60],
  ["d", 24 * 60 * 60],
  ["h", 60 * 60],
  ["m", 60],
  ["s", 1],
];
const PERIOD_LABEL_UNITS = [
  PERIOD_UNITS[0], PERIOD_UNITS[2], PERIOD_UNITS[3],
  PERIOD_UNITS[4], PERIOD_UNITS[5], PERIOD_UNITS[1],
];
const NUMBER_WORDS = new Map([
  ["one", 1], ["two", 2], ["three", 3], ["four", 4], ["five", 5],
  ["six", 6], ["seven", 7], ["eight", 8], ["nine", 9], ["ten", 10],
  ["eleven", 11], ["twelve", 12],
]);

function periodFromText(value) {
  const text = String(value || "").toLowerCase().replaceAll("_", "-");
  const compact = text.match(/(?:^|[^a-z0-9])(\d+)\s*(mo|w|d|h|m|s)(?:$|[^a-z0-9])/);
  if (compact) {
    const unit = PERIOD_UNITS.find(([suffix]) => suffix === compact[2]);
    const seconds = Number(compact[1]) * unit[1];
    return Number.isSafeInteger(seconds) && seconds > 0 && seconds <= RESET_PERIOD_LIMIT_SECONDS
      ? seconds
      : null;
  }

  const named = text.match(/(?:^|[^a-z0-9])(\d+|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve)[ -]?(months?|weeks?|days?|hours?|minutes?|seconds?)(?:$|[^a-z0-9])/);
  if (named) {
    const count = NUMBER_WORDS.get(named[1]) ?? Number(named[1]);
    const unit = named[2].startsWith("month")
      ? PERIOD_UNITS[0][1]
      : named[2].startsWith("week")
        ? PERIOD_UNITS[1][1]
        : named[2].startsWith("day")
          ? PERIOD_UNITS[2][1]
          : named[2].startsWith("hour")
            ? PERIOD_UNITS[3][1]
            : named[2].startsWith("minute")
              ? PERIOD_UNITS[4][1]
              : PERIOD_UNITS[5][1];
    const seconds = count * unit;
    return Number.isSafeInteger(seconds) && seconds > 0 && seconds <= RESET_PERIOD_LIMIT_SECONDS
      ? seconds
      : null;
  }

  for (const [pattern, seconds] of [
    [/\bmonthly\b/, PERIOD_UNITS[0][1]],
    [/\bweekly\b/, PERIOD_UNITS[1][1]],
    [/\bdaily\b/, PERIOD_UNITS[2][1]],
    [/\bhourly\b/, PERIOD_UNITS[3][1]],
  ]) {
    if (pattern.test(text)) return seconds;
  }
  return null;
}

function periodFromResetPoints(points) {
  const resetTimes = [...new Set((points || [])
    .map((point) => Number(point?.resetsAt))
    .filter((value) => Number.isSafeInteger(value) && value > 0))]
    .sort((left, right) => left - right);
  const counts = new Map();
  for (let index = 1; index < resetTimes.length; index += 1) {
    const difference = resetTimes[index] - resetTimes[index - 1];
    if (difference > 0 && difference <= RESET_PERIOD_LIMIT_SECONDS) {
      counts.set(difference, (counts.get(difference) || 0) + 1);
    }
  }
  return [...counts.entries()]
    .sort(([left, leftCount], [right, rightCount]) => rightCount - leftCount || left - right)
    .at(0)?.[0] || null;
}

export function quotaTierPeriodSeconds(metric, points = []) {
  return periodFromText(`${metric?.key || ""} ${metric?.label || ""}`)
    ?? periodFromResetPoints(points);
}

export function quotaTierPeriodLabel(seconds) {
  const value = Number(seconds);
  if (!Number.isSafeInteger(value) || value <= 0) return "—";
  for (const [suffix, unit] of PERIOD_LABEL_UNITS) {
    if (value % unit === 0) return `${value / unit}${suffix}`;
  }
  return `${value}s`;
}

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
  const { title, groups, selected, onChange, allLabel, noneLabel, showTitleInTrigger = true, itemControl } = options;
  const wasOpen = Boolean(host.querySelector(":popover-open"));
  const focusValue = host.ownerDocument.activeElement?.dataset?.value;
  const focusControl = host.ownerDocument.activeElement?.dataset?.control;
  host.replaceChildren();
  const trigger = document.createElement("button");
  trigger.type = "button";
  trigger.className = "quota-picker-trigger";
  const selectionLabel = selected === null ? allLabel : selected.length;
  trigger.textContent = showTitleInTrigger ? `${title}: ${selectionLabel}` : selectionLabel;
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
      input.dataset.control = "selection";
      input.checked = selected === null || selected.includes(item.value);
      input.addEventListener("change", () => {
        const next = new Set(selected === null ? values : selected);
        if (input.checked) next.add(item.value); else next.delete(item.value);
        update([...next]);
      });
      label.append(input, item.label);
      if (itemControl) {
        const row = document.createElement("div");
        row.className = "quota-picker-option";
        const controlLabel = document.createElement("label");
        controlLabel.className = "quota-picker-item-control";
        const control = document.createElement("input");
        control.type = "checkbox";
        control.dataset.value = item.value;
        control.dataset.control = "extra";
        control.checked = itemControl.selected.has(item.value);
        control.setAttribute("aria-label", `${item.label}: ${itemControl.label}`);
        control.addEventListener("change", () => itemControl.onChange(item.value, control.checked));
        controlLabel.append(control, itemControl.label);
        row.append(label, controlLabel);
        fieldset.append(row);
      } else {
        fieldset.append(label);
      }
    }
    menu.append(fieldset);
  }
  host.append(trigger, menu);
  if (wasOpen) {
    menu.showPopover();
    position();
    const focus = [...menu.querySelectorAll("input")].find((input) => input.dataset.value === focusValue
      && input.dataset.control === focusControl);
    (focus || trigger).focus({ preventScroll: true });
  }
}
