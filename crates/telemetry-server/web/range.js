export const DAY_SECONDS = 24 * 60 * 60;
// Keep browser validation aligned with the Server query limit.
export const MAX_RANGE_DAYS = 720;
export const MAX_RANGE_SECONDS = MAX_RANGE_DAYS * DAY_SECONDS;

export const RANGE_PRESETS = ["today", "1h", "24h", "7d", "14d", "30d", "1y", "last-reset", "all"];

export const BUCKET_PRESETS = [
  "1s",
  "1m",
  "5m",
  "15m",
  "30m",
  "1h",
  "2h",
  "6h",
  "12h",
  "1d",
  "1mo",
];

export function startOfLocalDayMs(timestampMs) {
  const date = new Date(timestampMs);
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

export function addLocalDaysMs(timestampMs, days) {
  const date = new Date(timestampMs);
  date.setDate(date.getDate() + days);
  return date.getTime();
}

export function defaultCustomRange(nowMs = Date.now()) {
  const endMs = startOfLocalDayMs(nowMs);
  return {
    from: Math.floor(addLocalDaysMs(endMs, -1) / 1000),
    to: Math.floor(endMs / 1000),
  };
}

export function resolvePresetRange(preset, nowMs = Date.now(), customRange = null) {
  const now = Math.floor(nowMs / 1000);
  switch (preset) {
    case "today":
      return { from: Math.floor(startOfLocalDayMs(nowMs) / 1000), to: now };
    case "1h":
      return { from: now - 60 * 60, to: now };
    case "7d":
    case "14d":
    case "30d": {
      const days = Number(preset.slice(0, -1));
      const startMs = addLocalDaysMs(startOfLocalDayMs(nowMs), -(days - 1));
      return { from: Math.floor(startMs / 1000), to: now };
    }
    case "1y": {
      const nowDate = new Date(nowMs);
      const year = nowDate.getFullYear() - 1;
      const month = nowDate.getMonth();
      const lastDay = new Date(year, month + 1, 0).getDate();
      const start = new Date(year, month, Math.min(nowDate.getDate(), lastDay));
      return { from: Math.floor(startOfLocalDayMs(start.getTime()) / 1000), to: now };
    }
    case "custom":
      return customRange ? { ...customRange } : defaultCustomRange(nowMs);
    default:
      return { from: now - DAY_SECONDS, to: now };
  }
}

export function resolveAllTimeRange(firstRecordedAt, nowMs = Date.now()) {
  const to = Math.floor(nowMs / 1000);
  const from = Number.isSafeInteger(firstRecordedAt) && firstRecordedAt >= 0
    ? Math.min(firstRecordedAt, to - 1) : to - 1;
  return { from, to };
}

export function resolveResetRange(resetsAt, periodSeconds, nowMs = Date.now()) {
  const reset = Number(resetsAt);
  const period = Number(periodSeconds);
  const now = Math.floor(Number(nowMs) / 1000);
  if (!Number.isSafeInteger(reset) || reset <= 0
    || !Number.isSafeInteger(period) || period <= 0 || period > MAX_RANGE_SECONDS
    || !Number.isSafeInteger(now) || now <= 0 || reset <= now) {
    return null;
  }
  const from = reset - period;
  if (!Number.isSafeInteger(from) || from < 0 || from >= now || now - from > MAX_RANGE_SECONDS) {
    return null;
  }
  return { from, to: now };
}

function pad(value) {
  return String(value).padStart(2, "0");
}

export function dateInputValue(timestamp) {
  const date = new Date(Number(timestamp) * 1000);
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

export function timeInputValue(timestamp, timeFormat = "24h") {
  const date = new Date(Number(timestamp) * 1000);
  if (timeFormat === "12h") {
    const hour = date.getHours();
    return `${pad(hour % 12 || 12)}:${pad(date.getMinutes())} ${hour >= 12 ? "PM" : "AM"}`;
  }
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function parseTimeParts(timeValue, timeFormat) {
  const value = String(timeValue || "").trim();
  if (timeFormat === "12h") {
    const match = value.match(/^(\d{1,2}):(\d{2})\s*(AM|PM)$/i);
    if (!match) return null;
    const hour = Number(match[1]);
    const minute = Number(match[2]);
    if (hour < 1 || hour > 12 || minute > 59) return null;
    const meridiem = match[3].toUpperCase();
    return [hour % 12 + (meridiem === "PM" ? 12 : 0), minute];
  }
  const match = value.match(/^(\d{1,2}):(\d{2})$/);
  if (!match) return null;
  const hour = Number(match[1]);
  const minute = Number(match[2]);
  return hour <= 23 && minute <= 59 ? [hour, minute] : null;
}

export function timeInputPlaceholder(timeFormat = "24h") {
  return timeFormat === "12h" ? "hh:mm AM/PM" : "HH:mm";
}

export function parseDateTimeParts(dateValue, timeValue, timeFormat = "24h") {
  const [year, month, day] = String(dateValue).split("-").map(Number);
  const time = parseTimeParts(timeValue, timeFormat);
  if (!time || ![year, month, day].every(Number.isFinite)) return NaN;
  const [hour, minute] = time;
  const date = new Date(year, month - 1, day, hour, minute, 0, 0);
  if (
    date.getFullYear() !== year ||
    date.getMonth() !== month - 1 ||
    date.getDate() !== day ||
    date.getHours() !== hour ||
    date.getMinutes() !== minute
  ) return NaN;
  return Math.floor(date.getTime() / 1000);
}

export function setDateKeepTime(timestamp, day) {
  const base = new Date(Number(timestamp) * 1000);
  return Math.floor(new Date(
    day.getFullYear(),
    day.getMonth(),
    day.getDate(),
    base.getHours(),
    base.getMinutes(),
    0,
    0,
  ).getTime() / 1000);
}

export function sameLocalDay(left, right) {
  return left.getFullYear() === right.getFullYear()
    && left.getMonth() === right.getMonth()
    && left.getDate() === right.getDate();
}

export function calendarDays(month) {
  const first = new Date(month.getFullYear(), month.getMonth(), 1);
  const start = new Date(first);
  start.setDate(first.getDate() - first.getDay());
  return Array.from({ length: 42 }, (_, index) => {
    const day = new Date(start);
    day.setDate(start.getDate() + index);
    return day;
  });
}

export function parseBucketValue(amount, unit) {
  const value = Number(amount);
  const multipliers = { s: 1, m: 60, h: 60 * 60, d: DAY_SECONDS };
  if (!Number.isInteger(value) || value < 1 || !multipliers[unit]) return null;
  const seconds = value * multipliers[unit];
  if (!Number.isSafeInteger(seconds) || seconds > MAX_RANGE_SECONDS) return null;
  return `${value}${unit}`;
}

export function splitBucketValue(bucket) {
  const match = String(bucket).match(/^(\d+)([smhd])$/);
  return match ? { amount: Number(match[1]), unit: match[2] } : null;
}

export function bucketDisplayLabel(bucket) {
  const parsed = splitBucketValue(bucket);
  if (!parsed) return bucket;
  return `${parsed.amount}${parsed.unit}`;
}
