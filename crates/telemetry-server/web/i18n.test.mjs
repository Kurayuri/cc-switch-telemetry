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

test("translations interpolate variables and formatters follow locale", () => {
  assert.equal(translate("en-US", "kpi.successCount", { count: 3 }), "3 successful");
  assert.match(createFormatters("zh-CN").integerNumber.format(1234), /1[,.]234/);
});
