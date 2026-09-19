#!/usr/bin/env python3
"""Optional UI regression: Python Playwright + cached Chromium + built release server.

Runs a temporary server/database, never the deployed instance.
Usage: python3 scripts/test-past-resets-browser.py
"""
import json
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
OLD_RESET = NOW - 72000
CURRENT_RESET = NOW + 9000


def seed(path):
    with sqlite3.connect(path) as db:
        for node in ["node-a", "node-b"]:
            db.execute("INSERT INTO nodes VALUES (?,?,'',?,?)", (node, node, NOW, NOW))
            db.execute("INSERT INTO quota_provider_states VALUES (?,'codex','shared','Shared','ok',NULL,?,?,?)", (node, NOW, NOW, NOW))
        def sample(node, key, reset, sampled, usage):
            observation = f"{node}-{key}-{sampled}"
            db.execute("INSERT INTO quota_observations VALUES (?,?,'test','codex','shared',?,?)", (node, observation, sampled, sampled))
            db.execute("INSERT INTO quota_metrics VALUES (?,?,?,?,'utilizationPercent',?,NULL,NULL,NULL,'%',?)", (node, observation, key, key, usage, reset))
        for offset in [0, 60, 120]:
            sample("node-a", "5h", OLD_RESET, OLD_RESET - 500 + offset, 25)
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
    errors = []
    requests = []
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.on("request", lambda request: requests.append(request.url))
    page.goto(origin + "/dashboard/")
    expect(page.locator("#refreshButton")).to_be_enabled()
    expect(page.locator("#errorBanner")).to_be_hidden()
    assert not any("/quota/resets" in url for url in requests), "history must be lazy"
    for field, value in [("nodeFilter", "node-b"), ("providerFilter", "usage-other"), ("modelFilter", "test-model")]:
        page.locator("#" + field).select_option(value)
        expect(page.locator("#refreshButton")).to_be_enabled()
    filters = {field: page.locator("#" + field).input_value() for field in ["nodeFilter", "providerFilter", "modelFilter"]}

    def open_past():
        page.locator("#rangePickerTrigger").click()
        if not page.locator("#pastResetEditor").is_visible():
            page.locator('[data-range-preset="past-resets"]').click()
        expect(page.locator("#pastResetCycle option")).to_have_count(2)
        expect(page.locator("#applyRange")).to_be_enabled()

    page.locator("#rangePickerTrigger").click()
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator('[data-range-preset="all"]').click()
    query = parse_qs(urlparse(requested.value.url).query)
    assert query["from"] == [str(NOW - 900 * 86400)]
    assert query["all_time"] == ["true"]
    assert query["node_id"] == ["node-b"]
    expect(page.locator("#refreshButton")).to_be_enabled()
    expect(page.locator("#rangePickerLabel")).to_have_text("All time")
    expect(page.locator("#errorBanner")).to_be_hidden()
    page.locator("#rangePickerTrigger").click()
    page.locator('[data-range-preset="24h"]').click()
    expect(page.locator("#refreshButton")).to_be_enabled()

    open_past()
    expect(page.locator("#pastResetProvider option")).to_have_count(2)
    expect(page.locator("#pastResetProvider option").first).to_contain_text("Alias A")
    expect(page.locator("#pastResetTier option")).to_have_count(2)
    expect(page.locator("#pastResetCycle option").first).to_contain_text("In progress")
    expect(page.locator("#rangePickerLabel")).to_have_text("Last 24 hours")
    page.locator("#pastResetTier").select_option(index=1)
    expect(page.locator("#pastResetCycle option")).to_have_count(1)
    page.locator("#pastResetTier").select_option(index=0)
    page.locator("#pastResetCycle").select_option(str(OLD_RESET))
    page.locator("#cancelRange").click()
    expect(page.locator("#rangePickerLabel")).to_have_text("Last 24 hours")

    open_past()
    page.locator("#pastResetCycle").select_option(str(OLD_RESET))
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator("#applyRange").click()
    query = parse_qs(urlparse(requested.value.url).query)
    assert query["from"] == [str(OLD_RESET - 18000)] and query["to"] == [str(OLD_RESET)]
    assert query["node_id"] == ["node-b"] and query["provider_id"] == ["usage-other"]
    assert query["model"] == ["test-model"]
    expect(page.locator("#refreshButton")).to_be_enabled()
    expect(page.locator("#rangePickerLabel")).to_have_text("Past Resets")
    for field, value in filters.items():
        assert page.locator("#" + field).input_value() == value
    expect(page.locator("#errorBanner")).to_be_hidden()

    history_requests = sum("/quota/resets" in url for url in requests)
    open_past()
    assert sum("/quota/resets" in url for url in requests) > history_requests
    assert page.locator("#pastResetCycle").input_value() == str(OLD_RESET)
    page.locator("#pastResetCycle").select_option(str(CURRENT_RESET))
    page.locator("#closeRangePicker").click()
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator("#refreshButton").click()
    assert parse_qs(urlparse(requested.value.url).query)["to"] == [str(OLD_RESET)]
    expect(page.locator("#refreshButton")).to_be_enabled()

    # A failure leaves the previously applied range intact, and Retry recovers it.
    route = "**/v3/dashboard/quota/resets"
    page.route(route, lambda request: request.fulfill(status=503, json={"message": "test failure"}))
    page.locator("#rangePickerTrigger").click()
    expect(page.locator("#pastResetRetry")).to_be_visible()
    expect(page.locator("#applyRange")).to_be_disabled()
    page.unroute(route)
    page.locator("#pastResetRetry").click()
    expect(page.locator("#applyRange")).to_be_enabled()
    assert page.locator("#pastResetCycle").input_value() == str(OLD_RESET)
    page.locator("#pastResetCycle").select_option(str(CURRENT_RESET))
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator("#applyRange").click()
    query = parse_qs(urlparse(requested.value.url).query)
    assert query["from"] == [str(CURRENT_RESET - 18000)]
    assert NOW <= int(query["to"][0]) < CURRENT_RESET
    expect(page.locator("#refreshButton")).to_be_enabled()

    open_past()
    page.screenshot(path=str(artifacts / "past-resets-desktop.png"))
    page.locator("#cancelRange").click()
    page.locator("#languageToggle").click()
    page.set_viewport_size({"width": 390, "height": 844})
    open_past()
    expect(page.locator("#pastResetCycle option").first).to_contain_text("进行中")
    expect(page.locator("#rangePickerLabel")).to_have_text("历史重置周期")
    page.locator("#applyRange").scroll_into_view_if_needed()
    assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), "mobile overflow"
    page.screenshot(path=str(artifacts / "past-resets-mobile.png"))
    page.locator("#cancelRange").click()

    page.route(route, lambda request: request.fulfill(json={"providers": []}))
    page.locator("#rangePickerTrigger").click()
    expect(page.locator("#pastResetStatus")).to_have_text("没有可用的非零用量重置周期。")
    expect(page.locator("#applyRange")).to_be_disabled()
    page.locator("#cancelRange").click()
    page.unroute(route)
    # A newly confirmed early reset must close the selected current cycle on refresh.
    history = page.request.get(origin + "/v3/dashboard/quota/resets").json()
    first_provider = next(p for p in history["providers"] if p["nodeId"] == "node-a")
    current_tier = next(t for t in first_provider["tiers"] if t["key"] == "5h")
    early_start = NOW - 200
    current_tier["resets"].append({"resetsAt": early_start + 18000,
        "firstSampledAt": NOW - 170, "lastSampledAt": NOW - 50,
        "sampleCount": 3, "firstUsageAt": None})
    page.route(route, lambda request: request.fulfill(json=history))
    with page.expect_request("**/v3/dashboard/overview?*") as requested:
        page.locator("#refreshButton").click()
    query = parse_qs(urlparse(requested.value.url).query)
    assert query["from"] == [str(CURRENT_RESET - 18000)]
    assert query["to"] == [str(early_start)]
    expect(page.locator("#refreshButton")).to_be_enabled()
    page.unroute(route)
    # The shared draft state must also preserve Last reset and Custom behavior.
    page.locator("#rangePickerTrigger").click()
    page.locator('[data-range-preset="last-reset"]').click()
    expect(page.locator("#applyRange")).to_be_enabled()
    page.locator("#applyRange").click()
    expect(page.locator("#refreshButton")).to_be_enabled()
    expect(page.locator("#rangePickerLabel")).to_have_text("最近一次重置")
    page.locator("#rangePickerTrigger").click()
    page.locator('[data-range-preset="custom"]').click()
    expect(page.locator("#customFromDate")).to_be_enabled()
    page.locator("#cancelRange").click()
    expect(page.locator("#rangePickerLabel")).to_have_text("最近一次重置")
    assert not errors, errors
    print("PASS: 900-day All time, lazy API, provider/tier/cycle choices, stable resets, aliases, cancel/close, early-cycle closure on refresh, filter preservation, retry/empty history, English/Chinese and mobile layout")


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
                    finally:
                        browser.close()
            finally:
                server.terminate()
                server.wait(timeout=10)


if __name__ == "__main__":
    main()
