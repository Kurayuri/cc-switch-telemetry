import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createFormatters, messages, resolveLocale, supportedLocales, translate } from "./i18n.js";

test("saved locale takes precedence over browser language", () => {
  assert.equal(resolveLocale("en-US", ["zh-CN"]), "en-US");
  assert.equal(resolveLocale("zh-CN", ["en-US"]), "zh-CN");
});

test("browser language selects Chinese and otherwise falls back to English", () => {
  assert.equal(resolveLocale(null, ["zh-Hans-CN", "en-US"]), "zh-CN");
  assert.equal(resolveLocale("invalid", ["fr-FR"]), "en-US");
});

test("both locales expose the same translation keys", () => {
  assert.deepEqual(
    Object.keys(messages["zh-CN"]).sort(),
    Object.keys(messages["en-US"]).sort(),
  );
  assert.deepEqual(Object.keys(messages).sort(), [...supportedLocales].sort());
});

test("every translation key used by the dashboard exists in both locales", () => {
  const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  const usedKeys = [...html.matchAll(/data-i18n(?:-aria-label)?="([^"]+)"/g)]
    .map((match) => match[1]);
  for (const key of usedKeys) {
    assert.ok(messages["zh-CN"][key], `missing zh-CN key: ${key}`);
    assert.ok(messages["en-US"][key], `missing en-US key: ${key}`);
  }
});

test("dashboard has an English static fallback without remote assets", () => {
  const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  assert.match(html, /<html lang="en-US"(?: data-theme="dark")?>/);
  assert.doesNotMatch(html, /[\u3400-\u9fff]/u);
  assert.doesNotMatch(html, /https?:\/\//u);
  assert.match(html, /\/dashboard\/styles\.css/);
  assert.match(html, /\/dashboard\/app\.js/);
});

test("dashboard defaults to dark and exposes a theme toggle", () => {
  const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");
  const app = readFileSync(new URL("./app.js", import.meta.url), "utf8");
  assert.match(html, /<html lang="en-US" data-theme="dark">/);
  assert.match(html, /id="themeToggle"/);
  assert.match(css, /:root\s*\{[\s\S]*color-scheme: dark/);
  assert.match(css, /\[data-theme="light"\]/);
  assert.doesNotMatch(css, /prefers-color-scheme/);
  assert.match(app, /cc-switch-telemetry\.theme/);
  assert.match(app, /themeToggle\.addEventListener/);
});

test("trend exposes metric and granularity controls with rich tooltip support", () => {
  const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  const app = readFileSync(new URL("./app.js", import.meta.url), "utf8");
  assert.match(html, /id="trendMetric"/);
  assert.match(html, /id="trendBucket"/);
  assert.match(html, /value="auto"/);
  assert.match(html, /value="1m"/);
  assert.match(html, /id="trendTooltip"/);
  assert.match(app, /params\.set\("bucket", elements\.trendBucket\.value\)/);
  assert.match(app, /trend\.tooltipCacheCreation/);
});

test("dashboard exposes compact KPI details, custom range dialog, and directional animations", () => {
  const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");
  const app = readFileSync(new URL("./app.js", import.meta.url), "utf8");
  assert.match(html, /class="kpi-heading"/);
  assert.match(html, /id="tokenComposition"/);
  assert.match(html, /id="customRangeDialog"/);
  assert.match(html, /id="customRangeForm"/);
  assert.match(app, /customRangeDialog\.showModal/);
  assert.match(app, /pathLength: 1/);
  assert.match(app, /requestAnimationFrame/);
  assert.match(css, /stroke-dasharray: 1/);
  assert.doesNotMatch(css, /@keyframes value-update\s*\{[^}]*transform:/s);
});

test("actual token KPI owns the cache rate and spans two grid units", () => {
  const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");
  assert.match(html, /class="cache-hit-inline"/);
  assert.match(html, /id="kpiCacheRate"/);
  assert.match(html, /class="token-bar-row"/);
  assert.doesNotMatch(html, /class="kpi-card cache-card"/);
  assert.match(css, /\.tokens-card\s*\{[^}]*grid-column:\s*span 2/);
  assert.match(css, /\.kpi-grid\s*\{[^}]*grid-template-columns:\s*repeat\(3/);
  assert.match(css, /\.token-summary\s*\{[^}]*font-size:\s*14px/);
  assert.match(css, /\.cache-hit-inline span\s*\{[^}]*font-size:\s*14px/);
  assert.match(css, /\.cache-hit-inline span\s*\{[^}]*position:\s*absolute/);
  assert.match(css, /\.cache-hit-inline\s*\{[^}]*min-height:\s*14px/);
  assert.match(css, /\.token-bar-row\s*\{[^}]*gap:\s*10px/);
  assert.match(css, /\.cache-hit-inline\s*\{[^}]*width:\s*108px/);
  assert.match(css, /\.token-legend\s*\{[^}]*flex-wrap:\s*nowrap/);
});

test("estimated cost exposes a dynamic top-three model list", () => {
  const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  const app = readFileSync(new URL("./app.js", import.meta.url), "utf8");
  assert.match(html, /id="kpiCostTopModels"/);
  assert.match(html, /data-i18n="kpi\.topModels"/);
  assert.match(app, /sort\(\(left, right\) => Number\(right\.totalCostUsd/);
  assert.match(app, /\.slice\(0, 3\)/);
  assert.match(app, /renderCostTopModels\(overview\.breakdowns\?\.models\)/);
});

test("translations interpolate variables and formatters follow locale", () => {
  assert.equal(translate("en-US", "kpi.successCount", { count: 3 }), "3 successful");
  assert.match(createFormatters("zh-CN").integerNumber.format(1234), /1[,.]234/);
});
