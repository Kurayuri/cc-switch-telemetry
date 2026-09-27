#!/usr/bin/env python3
"""Optional UI regression: Python Playwright + cached Chromium + built release server.

Runs a temporary server/database, never the deployed instance.
Usage: python3 scripts/test-past-resets-browser.py
"""
import json
from datetime import datetime, timedelta
from zoneinfo import ZoneInfo
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import time
from urllib.parse import parse_qs, urlparse
from urllib.request import ProxyHandler, build_opener

from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
NOW = int(time.time())
OLD_RESET = NOW - 9000
CURRENT_RESET = NOW + 9000


def seed(path):
    with sqlite3.connect(path) as db:
        for node in ["node-a", "node-b"]:
            db.execute("INSERT INTO nodes VALUES (?,?,'',?,?)", (node, node, NOW, NOW))
            db.execute("INSERT INTO quota_provider_states(node_id,app_type,provider_id,provider_name,status,target_kind,checked_at,last_success_at,received_at) VALUES (?,'codex','shared','Shared','ok',NULL,?,?,?)", (node, NOW, NOW, NOW))
        def sample(node, key, reset, sampled, usage):
            observation = f"{node}-{key}-{sampled}"
            db.execute("INSERT INTO quota_observations VALUES (?,?,'test','codex','shared',?,?)", (node, observation, sampled, sampled))
            db.execute("INSERT INTO quota_metrics VALUES (?,?,?,?,'utilizationPercent',?,NULL,NULL,NULL,'%',?)", (node, observation, key, key, usage, reset))
        for offset in [0, 60, 120]:
            sample("node-a", "5h", OLD_RESET, OLD_RESET - 500 + offset, 0)
            sample("node-a", "5h", CURRENT_RESET, NOW - 600 + offset, 10)
            sample("node-a", "7d", NOW + 604000, NOW - 600 + offset, 20)
            sample("node-b", "5h", CURRENT_RESET + 100, NOW - 600 + offset, 5)
        sample("node-a", "5h", CURRENT_RESET + 120, NOW - 90, 0)
        # All time must include records older than the ordinary 720-day query limit.
        db.execute("INSERT INTO quota_observations VALUES ('node-a','ancient','test','codex','shared',?,?)", (NOW - 900 * 86400, NOW - 900 * 86400))
        db.execute("""INSERT INTO usage_events
            (event_id,node_id,request_id,created_at,app_type,provider_id,model,
             input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,
             total_cost_usd,latency_ms,status_code,is_streaming,data_source,received_at)
            VALUES ('test','node-b','test',?,'codex','usage-other','test-model',10,5,0,0,'0.01',10,200,0,'test',?)""", (NOW - 100, NOW))


def check(page, origin, artifacts):
    errors, requests = [], []
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.on("request", lambda request: requests.append(request.url))
    page.goto(origin + "/dashboard/")
    expect(page.locator("#refreshButton")).to_be_enabled()
    assert not any("/quota/resets" in url for url in requests), "history must be lazy"
    expect(page.locator('[data-range-preset="past-resets"]')).to_have_count(0)
    for field, value in [("nodeFilter", "node-b"), ("providerFilter", "usage-other"), ("modelFilter", "test-model")]:
        page.locator("#" + field).select_option(value)
        expect(page.locator("#refreshButton")).to_be_enabled()
    filters = {field: page.locator("#" + field).input_value() for field in ["nodeFilter", "providerFilter", "modelFilter"]}

    def apply():
        with page.expect_request("**/v3/dashboard/overview?*") as requested:
            page.locator("#applyRange").click()
        expect(page.locator("#refreshButton")).to_be_enabled()
        return parse_qs(urlparse(requested.value.url).query)

    def open_cycle():
        page.locator("#rangePickerTrigger").click()
        if not page.locator("#lastResetEditor").is_visible():
            page.locator('[data-range-preset="last-reset"]').click()
        expect(page.locator("#resetCycleSelect option")).to_have_count(2)
        expect(page.locator("#applyRange")).to_be_enabled()

    page.locator("#rangePickerTrigger").click()
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator('[data-range-preset="all"]').click()
    query = parse_qs(urlparse(requested.value.url).query)
    assert query["from"] == [str(NOW - 900 * 86400)] and query["all_time"] == ["true"]
    expect(page.locator("#refreshButton")).to_be_enabled()
    page.locator("#rangePickerTrigger").click()
    page.locator('[data-range-preset="24h"]').click()
    expect(page.locator("#refreshButton")).to_be_enabled()
    open_cycle()
    expect(page.locator("#lastResetProvider option").first).to_contain_text("Alias A")
    expect(page.locator("#resetCycleCurrent")).to_have_attribute("aria-pressed", "true")
    expect(page.locator("#resetCycleSelect")).to_have_value(str(CURRENT_RESET))
    page.locator("#resetCycleLast").click()
    expect(page.locator("#resetCycleSelect")).to_have_value(str(OLD_RESET))
    page.locator("#lastResetTier").select_option(index=1)
    expect(page.locator("#applyRange")).to_be_disabled()
    page.locator("#lastResetTier").select_option(index=0)
    expect(page.locator("#resetCycleLast")).to_have_attribute("aria-pressed", "true")
    page.locator("#cancelRange").click()
    expect(page.locator("#rangePickerLabel")).to_have_text("Last 24 hours")
    open_cycle()
    expect(page.locator("#resetCycleCurrent")).to_have_attribute("aria-pressed", "true")
    page.locator("#resetCycleLast").click()
    query = apply()
    assert query["from"] == [str(OLD_RESET - 18000)] and query["to"] == [str(OLD_RESET)]
    assert query["node_id"] == ["node-b"] and query["provider_id"] == ["usage-other"]
    expect(page.locator("#rangePickerLabel")).to_have_text("Reset cycle")
    for field, value in filters.items(): expect(page.locator("#" + field)).to_have_value(value)
    open_cycle()
    expect(page.locator("#resetCycleLast")).to_have_attribute("aria-pressed", "true")
    # Manual selection removes shortcut mode, and survives closing/reopening.
    page.locator("#resetCycleSelect").select_option(str(CURRENT_RESET))
    apply()
    open_cycle()
    expect(page.locator("#resetCycleCurrent")).to_have_attribute("aria-pressed", "false")
    page.locator("#cancelRange").click()
    route = "**/v3/dashboard/quota/resets*"
    # Cancelling an in-flight picker must retain the applied manual range.
    held = []
    page.route(route, lambda request: held.append(request))
    page.locator("#rangePickerTrigger").click()
    expect(page.locator("#lastResetError")).to_contain_text("Loading")
    expect(page.locator("#applyRange")).to_be_disabled()
    page.locator("#cancelRange").click()
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator("#modelFilter").select_option("")
    expect(page.locator("#refreshButton")).to_be_enabled()
    assert parse_qs(urlparse(requested.value.url).query)["from"] == [str(CURRENT_RESET - 18000)]
    assert held
    held[0].fulfill(json={"providers": []})
    page.unroute(route)
    expect(page.locator("#rangePickerLabel")).to_have_text("Reset cycle")
    page.locator("#modelFilter").select_option("test-model")
    expect(page.locator("#refreshButton")).to_be_enabled()
    history = page.request.get(origin + "/v3/dashboard/quota/resets").json()
    provider = next(p for p in history["providers"] if p["nodeId"] == "node-a")
    tier = next(t for t in provider["tiers"] if t["key"] == "5h")
    early = NOW - 200
    tier["resets"].append({"resetsAt": early + 18000, "firstSampledAt": NOW - 170,
        "lastSampledAt": NOW - 50, "sampleCount": 3, "firstUsageAt": None})
    page.route(route, lambda request: request.fulfill(json=history))
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator("#refreshButton").click()
    expect(page.locator("#refreshButton")).to_be_enabled()
    query = parse_qs(urlparse(requested.value.url).query)
    assert query["from"] == [str(CURRENT_RESET - 18000)] and query["to"] == [str(early)]
    page.locator("#rangePickerTrigger").click()
    expect(page.locator("#resetCycleSelect option")).to_have_count(3)
    page.locator("#resetCycleCurrent").click()
    query = apply()
    assert query["from"] == [str(early)] and int(query["to"][0]) >= NOW
    # Current follows another reset, previous follows it too; all without changing Usage filters.
    later = NOW - 20
    tier["resets"].append({"resetsAt": later + 18000, "firstSampledAt": later,
        "lastSampledAt": later + 120, "sampleCount": 3, "firstUsageAt": None})
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator("#refreshButton").click()
    expect(page.locator("#refreshButton")).to_be_enabled()
    assert parse_qs(urlparse(requested.value.url).query)["from"] == [str(later)]
    page.locator("#rangePickerTrigger").click()
    expect(page.locator("#resetCycleSelect option")).to_have_count(4)
    page.locator("#resetCycleLast").click()
    query = apply()
    assert query["from"] == [str(early)] and query["to"] == [str(later)]
    before = len([url for url in requests if "/quota/resets" in url])
    page.locator("#modelFilter").select_option("")
    expect(page.locator("#refreshButton")).to_be_enabled()
    assert len([url for url in requests if "/quota/resets" in url]) == before
    page.unroute(route)
    # Failed loads cannot apply stale data; retry recovers.
    page.route(route, lambda request: request.fulfill(status=503,json={"message":"test failure"}))
    page.locator("#rangePickerTrigger").click()
    expect(page.locator("#resetCycleRetry")).to_be_visible()
    expect(page.locator("#applyRange")).to_be_disabled()
    page.unroute(route)
    page.locator("#resetCycleRetry").click()
    expect(page.locator("#applyRange")).to_be_enabled()
    # Provider with no adjacent history: unavailable; switching from manual resets to current.
    page.locator("#lastResetProvider").select_option(index=1)
    expect(page.locator("#applyRange")).to_be_disabled()
    page.locator("#resetCycleCurrent").click()
    expect(page.locator("#applyRange")).to_be_enabled()
    page.locator("#lastResetProvider").select_option(index=0)
    page.locator("#resetCycleSelect").select_option(str(OLD_RESET))
    page.locator("#lastResetTier").select_option(index=1)
    expect(page.locator("#resetCycleCurrent")).to_have_attribute("aria-pressed", "true")
    page.locator("#lastResetTier").select_option(index=0)
    for lang in ["en-US", "zh-CN"]:
        if page.locator('html').get_attribute('lang') != lang:
            page.locator('#cancelRange').click()
            page.locator('#languageToggle').click()
            page.locator('#rangePickerTrigger').click()
            expect(page.locator('#applyRange')).to_be_enabled()
        for width in [1440,390]:
            page.set_viewport_size({"width":width,"height":1000})
            page.locator('#resetCycleLast').scroll_into_view_if_needed()
            assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
            page.locator('#rangePickerDialog').screenshot(path=str(artifacts/f'reset-cycle-{lang}-{width}.png'))
    page.locator("#cancelRange").click()
    expect(page.locator("#rangePickerLabel")).to_have_text("重置周期")
    assert not errors, errors
    print("PASS: Reset cycle defaults/cancel/manual/zero-use last/early reset/follow/filter isolation/retry/identity/layout")



def check_custom(page, origin, artifacts, time_format):
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))
    settings = page.request.get(origin + "/v3/dashboard/settings").json()
    settings.setdefault("dashboardDefaults", {})["timeFormat"] = time_format
    page.route("**/v3/dashboard/settings", lambda route: route.fulfill(json=settings))
    page.goto(origin + "/dashboard/")
    expect(page.locator("#refreshButton")).to_be_enabled()
    today = datetime.now(ZoneInfo("Asia/Shanghai")).date()
    midnight = "12:00:00 AM" if time_format == "12h" else "00:00:00"
    day_end = "11:59:59 PM" if time_format == "12h" else "23:59:59"

    def open_custom():
        page.locator("#rangePickerTrigger").click()
        page.locator('[data-range-preset="custom"]').click()

    def dates(start, end):
        expect(page.locator("#customFromDate")).to_have_value(start.isoformat())
        expect(page.locator("#customToDate")).to_have_value(end.isoformat())
        expect(page.locator("#customFromTime")).to_have_value(midnight)
        expect(page.locator("#customToTime")).to_have_value(day_end)

    def click_day(number):
        page.locator("#calendarDays .calendar-day:not(.outside-month)").get_by_text(str(number), exact=True).click()

    open_custom()
    dates(today, today)
    # Repeated range selection must move the start on the third click.
    month = today.replace(day=1)
    click_day(5)
    click_day(10)
    dates(month.replace(day=5), month.replace(day=10))
    click_day(20)
    click_day(15)
    dates(month.replace(day=15), month.replace(day=20))
    click_day(8)
    click_day(8)
    dates(month.replace(day=8), month.replace(day=8))
    # Focus, including keyboard navigation, explicitly selects an endpoint.
    page.locator("#customFromDate").focus()
    click_day(4)
    dates(month.replace(day=4), month.replace(day=8))
    page.locator('[data-range-field="end"] > span').click()
    click_day(12)
    dates(month.replace(day=4), month.replace(day=12))
    # Updating dates by hand updates the highlighted calendar endpoint.
    page.locator("#customFromDate").fill(month.replace(day=3).isoformat())
    page.locator("#customToDate").focus()
    expect(page.locator("#calendarDays .endpoint:not(.outside-month)").get_by_text("3", exact=True)).to_be_visible()
    # A new start in another month must not jump the calendar back.
    page.locator("#customFromDate").focus()
    page.locator("#previousCalendarMonth").click()
    previous = month - timedelta(days=1)
    heading = page.locator("#calendarMonthLabel").inner_text()
    click_day(previous.day)
    expect(page.locator("#calendarMonthLabel")).to_have_text(heading)
    page.locator("#nextCalendarMonth").click()
    heading = page.locator("#calendarMonthLabel").inner_text()
    click_day(2)
    expect(page.locator("#calendarMonthLabel")).to_have_text(heading)
    dates(previous, month.replace(day=2))
    page.screenshot(path=str(artifacts / f"custom-range-{time_format}-desktop.png"))
    # Manual seconds survive apply and reopening.
    manual_time = "11:58:47 PM" if time_format == "12h" else "23:58:47"
    page.locator("#customToTime").fill(manual_time)
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator("#applyRange").click()
    query = parse_qs(urlparse(requested.value.url).query)
    tz = ZoneInfo("Asia/Shanghai")
    expected_from = int(datetime.combine(previous, datetime.min.time(), tz).timestamp())
    expected_to = int(datetime(month.year, month.month, 2, 23, 58, 47, tzinfo=tz).timestamp())
    assert query["from"] == [str(expected_from)] and query["to"] == [str(expected_to)], query
    expect(page.locator("#refreshButton")).to_be_enabled()
    open_custom()
    expect(page.locator("#customToTime")).to_have_value(manual_time)
    click_day(previous.day)
    click_day(previous.day)
    dates(previous, previous)
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator("#applyRange").click()
    query = parse_qs(urlparse(requested.value.url).query)
    assert int(query["to"][0]) == expected_from + 86400 - 1, query
    expect(page.locator("#refreshButton")).to_be_enabled()
    # Applying a reset range must not overwrite saved custom dates/times.
    page.locator("#rangePickerTrigger").click()
    page.locator('[data-range-preset="last-reset"]').click()
    expect(page.locator("#resetCycleSelect")).to_have_value(str(CURRENT_RESET))
    page.locator("#applyRange").click()
    expect(page.locator("#refreshButton")).to_be_enabled()
    open_custom()
    dates(previous, previous)
    page.set_viewport_size({"width": 390, "height": 844})
    page.locator("#customToTime").scroll_into_view_if_needed()
    assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), "custom mobile overflow"
    assert page.locator("#customToTime").evaluate("el => el.scrollWidth <= el.clientWidth"), "time text clipped"
    page.screenshot(path=str(artifacts / f"custom-range-{time_format}-mobile.png"))
    page.locator("#cancelRange").click()
    assert not errors, errors
    print(f"PASS: Custom {time_format}, today defaults, repeat/reverse/same-day/cross-month selections, input sync, exact seconds, saved range isolation, mobile layout")


def main():
    with tempfile.TemporaryDirectory(prefix="past-resets-browser-") as directory:
        work = Path(directory)
        database = work / "telemetry.db"
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        origin = f"http://127.0.0.1:{port}"
        (work / "settings.json").write_text(json.dumps({"version": 1, "quotaDefaults": {"providers": None}, "quotaProviderAliases": [{"nodeId": "node-a", "providerId": "shared", "alias": "Alias A"}]}))
        environment = dict(os.environ, TELEMETRY_DB=str(database), TELEMETRY_LISTEN=f"127.0.0.1:{port}")
        environment.pop("ADMIN_PASSWORD", None)
        with (work / "server.log").open("w") as log:
            server = subprocess.Popen([str(ROOT / "target/release/telemetry-server")], env=environment, stdout=log, stderr=log)
            try:
                http = build_opener(ProxyHandler({}))
                for _ in range(100):
                    if server.poll() is not None:
                        raise RuntimeError("temporary server failed to start")
                    try:
                        with http.open(origin + "/healthz", timeout=1):
                            break
                    except OSError:
                        time.sleep(0.1)
                else:
                    raise RuntimeError("temporary server was not ready")
                seed(database)
                artifacts = ROOT / "artifacts/past-resets-tests"
                artifacts.mkdir(parents=True, exist_ok=True)
                with sync_playwright() as playwright:
                    browser = playwright.chromium.launch(headless=True)
                    try:
                        page = browser.new_page(locale="en-US", viewport={"width": 1440, "height": 1000})
                        check(page, origin, artifacts)
                        for time_format in ["24h", "12h"]:
                            custom_page = browser.new_page(locale="en-US", timezone_id="Asia/Shanghai", viewport={"width": 1440, "height": 1000})
                            check_custom(custom_page, origin, artifacts, time_format)
                            custom_page.close()
                    finally:
                        browser.close()
            finally:
                server.terminate()
                server.wait(timeout=10)


if __name__ == "__main__":
    main()
