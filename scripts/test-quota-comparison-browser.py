#!/usr/bin/env python3
"""Isolated browser regression using real Dashboard APIs and a temporary database."""
import json
import os
import re
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
START = (NOW // 300) * 300 - 3900  # Keep every fixture event safely in the past.
RESET = START + 18000
PROVIDER = '["node-a","shared"]'
TIER = '["5h","utilizationPercent","%"]'


def seed(path):
    with sqlite3.connect(path) as db:
        for node in ["node-a", "node-b", "node-c"]:
            db.execute("INSERT INTO nodes VALUES (?,?,'',?,?)", (node, node, NOW, NOW))
            db.execute("INSERT INTO quota_provider_states(node_id,app_type,provider_id,provider_name,status,target_kind,checked_at,last_success_at,received_at) VALUES (?,'codex','shared','Shared','ok',NULL,?,?,?)", (node, NOW, NOW, NOW))
            for step in range(61):
                at = START + step * 60
                for key, reset in [("5h", RESET), ("7d", START + 604800)]:
                    oid = f"{node}-{key}-{at}"
                    db.execute("INSERT INTO quota_observations VALUES (?,?,'test','codex','shared',?,?)", (node, oid, at, at))
                    db.execute("INSERT INTO quota_metrics VALUES (?,?,?,?,'utilizationPercent',?,NULL,NULL,NULL,'%',?)", (node, oid, key, key, step, reset))
                eid = f"{node}-{at}"
                db.execute("""INSERT INTO usage_events
                    (event_id,node_id,request_id,created_at,app_type,provider_id,model,
                    input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,
                    total_cost_usd,latency_ms,status_code,is_streaming,data_source,received_at)
                    VALUES (?,?,?,?,'codex',?,'test-model',100,50,0,0,'0.01',10,200,0,'test',?)""", (eid, node, eid, at + 10, '_codex_session' if node == 'node-c' else 'shared', NOW))
        # Direct SQL fixtures must mirror ingestion's mark_event_dirty call.
        # Otherwise full-hour queries can omit fixtures at an exact hour boundary.
        db.execute("""INSERT INTO usage_cache_partitions(node_id,hour_start,state,updated_at)
            SELECT node_id,created_at-(created_at%3600),'dirty',0 FROM usage_events
            GROUP BY node_id,created_at-(created_at%3600)
            ON CONFLICT(node_id,hour_start) DO UPDATE SET state='dirty'""")


def disable_history(page):
    page.locator('#historyReferenceEdit').click()
    page.locator('#historyReferenceSource').select_option('none')
    page.locator('#historyReferenceApply').click()
    expect(page.locator('#historyReferenceEdit')).to_contain_text('None')


def check(page, origin, artifacts):
    errors, requests, responses = [], [], {}
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.on("request", lambda request: requests.append(request.url))
    def capture(request):
        response = request.response()
        path = urlparse(request.url).path
        if path in ["/v3/dashboard/overview", "/v3/dashboard/quota"] and response.ok:
            responses[path] = response.json()
    page.on("requestfinished", capture)
    page.goto(origin + "/dashboard/")
    expect(page.locator("#refreshButton")).to_be_enabled()
    expect(page.locator("#errorBanner")).to_be_hidden()
    # Deliberately use a Usage scope different from the Quota provider.
    page.locator("#nodeFilter").select_option("node-c")
    expect(page.locator("#refreshButton")).to_be_enabled()
    page.locator("#providerFilter").select_option("_codex_session")
    expect(page.locator("#refreshButton")).to_be_enabled()
    page.locator("#trendMetric").select_option("totalRequests")
    page.locator("#trendCumulative").uncheck()
    chart = "(await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#trendChart'))"
    def chart_data():
        page.evaluate("() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))")
        return page.evaluate(f"async () => {{ const o = ({chart}).getOption(); return {{axes: o.yAxis.length, series: o.series.filter(Boolean).map(s=>({{id:s.id,data:s.data.map(d=>d.value)}}))}}; }}")
    def assert_usage_axis():
        assert page.evaluate(f"""async () => {{
          const c = ({chart});
          const o = c.getOption();
          const maximum = Math.max(0, ...o.series[0].data.map(d => d.value[1])) || 1;
          const extent = c.getModel().getComponent('yAxis', 0).axis.scale.getExtent();
          return o.yAxis[0].max === maximum && extent[0] === 0 && extent[1] === maximum;
        }}"""), "Usage axis must end at the displayed maximum"
    def assert_comparison_alignment():
        result = page.evaluate(f"""async (bounds) => {{
          const o = ({chart}).getOption();
          const usage = o.series.find(s => s?.id === 'usage-trend').data.filter(d => Number.isFinite(d.value[1]));
          const quota = o.series.find(s => s?.id === 'comparison-quota').data.filter(d => Number.isFinite(d.value[1]));
          let anchor = null;
          for (let index = usage.length - 1; index >= 0 && !anchor; index--) {{
            const from = usage[index].value[0] / 1000;
            const to = Math.min(from + bounds.bucketSeconds, bounds.rangeTo);
            const sample = quota.filter(d => d.value[0] / 1000 >= from && d.value[0] / 1000 < to).at(-1);
            if (sample) anchor = {{ usage: usage[index].value[1], utilization: sample.value[1] }};
          }}
          if (!anchor || !(anchor.usage > 0) || !(anchor.utilization > 0)) return {{ aligned: false, reason: 'no positive anchor' }};
          const usageRatio = anchor.usage / o.yAxis[0].max;
          const utilizationRatio = anchor.utilization / o.yAxis[1].max;
          const aligned = Math.abs(usageRatio - utilizationRatio) < 1e-9;
          const contained = o.series.every(s => s.data.every(d => d.value[1] == null
            || d.value[1] <= o.yAxis[s.yAxisIndex || 0].max + 1e-12));
          return {{ aligned, contained, usageRatio, utilizationRatio,
            usageMaximum: o.yAxis[0].max, utilizationMaximum: o.yAxis[1].max, anchor }};
        }}""", {"bucketSeconds": responses["/v3/dashboard/quota"]["range"]["bucketSeconds"],
                 "rangeTo": responses["/v3/dashboard/overview"]["range"]["to"]})
        assert result.get("aligned") and result.get("contained"), result
    def toggle_compare(enabled):
        bucket = "quota-auto" if enabled else "auto"
        with page.expect_response(lambda r: "/overview?" in r.url and parse_qs(urlparse(r.url).query).get("bucket") == [bucket]) as result:
            page.locator("#trendCompare").set_checked(enabled)
        overview = result.value.json()
        responses["/v3/dashboard/overview"] = overview
        expect(page.locator("#resolvedBucket")).to_contain_text(overview["range"]["bucket"])
        return overview
    regular_bucket = responses["/v3/dashboard/overview"]["range"]["bucket"]
    assert_usage_axis()
    normal_height = page.evaluate(f"async () => ({chart}).getModel().getComponent('grid').coordinateSystem.getRect().height")
    before = len(requests)
    toggle_compare(True)
    disable_history(page)
    expect(page.locator("#trendMetric")).to_have_value("totalCostUsd")
    expect(page.locator("#comparisonProvider")).to_have_value(PROVIDER)
    expect(page.locator("#comparisonTier")).to_have_value(TIER)
    expect(page.locator("#trendCumulative")).to_be_checked()
    expect(page.locator("#trendCumulative")).to_be_enabled()
    expect(page.locator("#comparisonPredict")).to_be_checked()
    page.locator("#comparisonPredict").uncheck()
    expect(page.locator("#comparisonPredict")).not_to_be_checked()
    expect(page.locator("#comparisonEstimatedQuota")).to_be_checked()
    page.locator("#comparisonEstimatedQuota").uncheck()
    expect(page.locator("#providerFilter")).to_have_value("_codex_session")
    expect(page.locator("#comparisonStatus")).to_be_hidden()
    new_requests = requests[before:]
    assert sum("/overview?" in url for url in new_requests) == 1
    assert not any("/quota?" in url for url in new_requests)
    assert regular_bucket == "1m", "ordinary Auto should use denser buckets for this one-hour range"
    assert page.locator("#comparisonUsageProvider, #comparisonSummary, #comparisonBrush").count() == 0
    option = chart_data()
    assert option["axes"] == 2
    assert page.evaluate(f"async () => ({chart}).getModel().getComponent('grid').coordinateSystem.getRect().height") >= normal_height
    assert page.locator("#trendChart").bounding_box()["height"] >= 320
    quota_values = page.evaluate("""async () => {
      const c = (await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#quotaChart'));
      return c.getOption().series.find(s=>JSON.parse(s.id).slice(0,3).join('/')==='node-a/shared/5h').data.map(d=>d.value);
    }""")
    assert option["series"][1]["data"] == quota_values, "overlay must match quota history exactly"
    assert page.evaluate(f"async () => ({chart}).getOption().yAxis[1].max") == max(p[1] for p in quota_values if p[1] is not None)
    assert_comparison_alignment()
    running, expected = 0, []
    overview = responses["/v3/dashboard/overview"]
    assert overview["range"]["bucket"] == responses["/v3/dashboard/quota"]["range"]["bucket"]
    raw_cost = [[p["bucketStart"] * 1000, p["totalCostUsd"]] for p in overview["trend"]]
    page.locator("#trendCumulative").uncheck()
    option = chart_data()
    assert option["series"][0]["data"] == raw_cost
    assert_comparison_alignment()
    page.locator("#trendCumulative").check()
    option = chart_data()
    for point in overview["trend"]:
        running += point["totalCostUsd"]
        expected.append([point["bucketStart"] * 1000, running])
    assert option["series"][0]["data"] == expected, "Usage must retain all original buckets"
    assert_comparison_alignment()
    page.locator("#trendCumulative").uncheck()
    assert chart_data()["series"][0]["data"] == raw_cost
    page.remove_listener("requestfinished", capture)
    before = len(requests)
    page.locator("#comparisonTier").select_option('["7d","utilizationPercent","%"]')
    page.locator("#comparisonProvider").select_option('["node-b","shared"]')
    page.locator("#comparisonTier").select_option(TIER)
    assert len(requests) == before, "selection must not fetch"
    toggle_compare(False)
    expect(page.locator("#trendMetric")).to_have_value("totalRequests")
    expect(page.locator("#trendCumulative")).not_to_be_checked()
    assert chart_data()["axes"] == 1
    assert responses["/v3/dashboard/overview"]["range"]["bucket"] == regular_bucket
    before = len(requests)
    toggle_compare(True)
    expect(page.locator("#trendMetric")).to_have_value("totalCostUsd")
    expect(page.locator("#trendCumulative")).to_be_checked()
    expect(page.locator("#comparisonPredict")).to_be_checked()
    page.locator("#trendChart").scroll_into_view_if_needed()
    page.wait_for_timeout(800)
    page.screenshot(path=str(artifacts / "comparison-desktop.png"))
    page.evaluate(f"async () => ({chart}).dispatchAction({{type:'showTip',seriesIndex:1,dataIndex:1}})")
    quota_tooltip = page.locator(".echarts-tooltip").filter(has_text="5h")
    expect(quota_tooltip).to_be_visible()
    assert not re.search(r"\d{1,2}:\d{2}:\d{2}", quota_tooltip.locator("strong").last.inner_text())
    # Compare defaults Predict on, sharing the exact forecast with the lower chart.
    forecast = page.evaluate(f"async () => ({chart}).getOption().series.find(s=>s?.id==='comparison-quota:predict').data.map(d=>d.value)")
    lower_forecast = page.evaluate("""async () => {
      const c = (await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#quotaChart'));
      return c.getOption().series.find(s=>s?.id.endsWith(':predict') && JSON.parse(s.id.slice(0,-8)).slice(0,3).join('/')==='node-a/shared/5h').data.map(d=>d.value);
    }""")
    assert forecast == lower_forecast
    assert page.evaluate(f"async () => ({chart}).getOption().xAxis[0].max") >= forecast[-1][0]
    page.wait_for_function("""async () => {
      const c = (await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#trendChart'));
      return c.getOption().series.some(s => s?.id === 'estimated-quota');
    }""")
    assert any('/quota/resets' in url for url in requests[before:])
    estimated = page.evaluate(f"async () => ({{data: ({chart}).getOption().series.find(s=>s?.id==='estimated-quota').data}})")["data"]
    valid_estimates = [point for point in estimated if point["value"][1] is not None]
    assert valid_estimates
    for point in valid_estimates:
        source = point["estimate"]
        expected_cost = .01 * sum(START + step * 60 + 10 < source["at"] for step in range(61))
        assert abs(source["cost"] - expected_cost) < 1e-9
        assert abs(point["value"][1] - expected_cost / (source["utilization"] / 100)) < 1e-9
    for metric in ['totalRequests', 'realTotalTokens', 'totalCostUsd']:
        page.locator('#trendMetric').select_option(metric)
        assert chart_data()['axes'] == (2 if metric == 'totalCostUsd' else 3)
        assert page.evaluate(f"async () => ({chart}).getOption().series.find(s=>s?.id==='estimated-quota').data") == estimated
    page.locator('#trendCumulative').uncheck()
    assert page.evaluate(f"async () => ({chart}).getOption().series.find(s=>s?.id==='estimated-quota').data") == estimated
    page.locator('#trendCumulative').check()
    expect(page.locator('#estimatedQuotaStatus')).to_be_hidden()
    assert_comparison_alignment()
    assert page.locator('#comparisonControls').evaluate("e => getComputedStyle(e).justifyContent") == 'flex-end'
    page.locator('#trendMetric').select_option('totalRequests')
    page.set_viewport_size({'width': 390, 'height': 844})
    page.locator('#trendChart').scroll_into_view_if_needed()
    page.wait_for_timeout(800)
    page.mouse.move(0, 0)
    page.evaluate(f"async () => ({chart}).dispatchAction({{type:'hideTip'}})")
    page.wait_for_timeout(400)
    page.locator('#trendChart').locator('..').screenshot(path=str(artifacts / 'estimated-quota-mobile.png'))
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    page.set_viewport_size({'width': 1440, 'height': 1000})
    page.locator('#trendChart').scroll_into_view_if_needed()
    page.wait_for_timeout(400)
    page.locator('#trendChart').locator('..').screenshot(path=str(artifacts / 'estimated-quota-desktop.png'))
    page.locator('#trendMetric').select_option('totalCostUsd')
    lower_predict = page.locator('#quotaMetricFilter input[data-control="extra"]').first
    expect(lower_predict).to_be_checked()
    page.locator("#quotaMetricFilter .quota-picker-trigger").click()
    lower_predict.uncheck()
    expect(page.locator("#comparisonPredict")).not_to_be_checked()
    page.keyboard.press("Escape")
    expect(page.locator("#comparisonEstimatedQuota")).to_be_checked()
    assert any(s["id"] == "estimated-quota" for s in chart_data()["series"])
    page.locator("#comparisonEstimatedQuota").uncheck()
    assert len(chart_data()["series"]) == 2
    # Reset-cycle refresh revalidates the selected provider boundaries before shared dashboard queries.
    requests.clear()
    page.locator("#refreshButton").click()
    expect(page.locator("#refreshButton")).to_be_enabled()
    paths = [urlparse(url).path for url in requests if "/v3/dashboard/" in url]
    assert paths.count("/v3/dashboard/overview") == 1, paths
    assert paths.count("/v3/dashboard/quota") == 2, paths
    assert paths.count("/v3/dashboard/filters") == 1, paths
    assert paths.count("/v3/dashboard/quota/resets") == 1, paths
    current_queries = [parse_qs(urlparse(url).query) for url in requests if "/quota?" in url and "include_history=false" in url]
    assert len(current_queries) == 1 and "node_id" in current_queries[0] and "provider_id" in current_queries[0]
    expect(page.locator("#comparisonStatus")).to_be_hidden()
    # Both controls change both endpoints, including the shared Auto rule.
    for trigger, bucket in [("trendBucketTrigger", "15m"), ("quotaBucketTrigger", "30m"), ("trendBucketTrigger", "auto")]:
        requests.clear()
        page.locator("#" + trigger).click()
        with page.expect_response(lambda r: "/overview?" in r.url and parse_qs(urlparse(r.url).query).get("bucket") == ["quota-auto" if bucket == "auto" else bucket]) as usage_response:
            with page.expect_response(lambda r: "/quota?" in r.url and parse_qs(urlparse(r.url).query).get("bucket") == [bucket]) as quota_response:
                page.locator(f'[data-bucket="{bucket}"]').click()
        usage_range = usage_response.value.json()["range"]
        quota_range = quota_response.value.json()["range"]
        assert usage_range["bucket"] == quota_range["bucket"], (usage_range, quota_range)
        expect(page.locator("#resolvedBucket")).to_contain_text(usage_range["bucket"])
        expect(page.locator("#quotaBucket")).to_contain_text(quota_range["bucket"])
        assert page.locator("#trendBucketLabel").inner_text() == page.locator("#quotaGranularityLabel").inner_text()
        assert sum("/overview?" in url for url in requests) == 1
        assert sum("/quota?" in url for url in requests) == 1
    # Exercise both completion orders after a changed time range. The old chart
    # stays visible until the held response arrives; mixed ranges never render.
    for delayed in ["overview", "quota"]:
        held = []
        pattern = f"**/v3/dashboard/{delayed}?*"
        page.route(pattern, lambda route: held.append(route))
        old = chart_data()
        page.locator("#rangePickerTrigger").click()
        preset = "7d" if delayed == "overview" else "24h"
        page.locator(f'[data-range-preset="{preset}"]').click()
        page.wait_for_timeout(500)
        assert held, delayed
        expect(page.locator("#trendChart")).to_be_visible()
        assert chart_data() == old, "retain complete previous chart while a new range is pending"
        for route in held:
            route.continue_()
        page.unroute(pattern)
        expect(page.locator("#refreshButton")).to_be_enabled()
        expect(page.locator("#comparisonStatus")).to_be_hidden()
        assert chart_data()["axes"] == 2
    page.locator("#languageToggle").click()
    page.set_viewport_size({"width": 390, "height": 844})
    page.locator("#comparisonControls").scroll_into_view_if_needed()
    page.screenshot(path=str(artifacts / "comparison-mobile.png"))
    assert page.evaluate("document.documentElement.scrollWidth <= innerWidth"), "mobile overflow"
    expect(page.locator("#trendCumulative")).to_be_enabled()
    assert page.locator("#trendChart").bounding_box()["height"] >= 320
    assert not errors, errors
    print("PASS: aligned comparison axes, cycle estimated quota values, three metric axes, Predict default/removal, responsive layout; cumulative and Predict enabled on each Compare entry and freely toggleable, coarse ordinary Auto/fine Compare Auto, minute Quota tooltip, shared Predict and preserved plot height")


def check_controls(page, origin, artifacts):
    errors = []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.emulate_media(reduced_motion='reduce')
    page.goto(origin + '/dashboard/')
    expect(page.locator('#refreshButton')).to_be_enabled()
    expect(page.locator('#kpiCacheRate')).to_have_text('0.00%')
    left = page.locator('.trend-controls').bounding_box()
    toolbar = page.locator('.trend-toolbar').bounding_box()
    assert abs(left['x'] - toolbar['x']) < 1, 'regular controls must start at the left'
    predict = page.locator('#comparisonPredict')
    estimate = page.locator('#comparisonEstimatedQuota')
    compare = page.locator('#trendCompare')

    def geometry():
        return page.evaluate('''() => Object.fromEntries(
          ['.trend-heading', '.trend-heading > h2', '.trend-toolbar', '.trend-controls', '#comparisonControls', '#trendMetric', '#trendCompare', '#trendBucketTrigger'].map(selector => {
            const r = document.querySelector(selector).getBoundingClientRect();
            const panel = document.querySelector('.chart-panel').getBoundingClientRect();
            return [selector, {x:r.x-panel.x, y:r.y-panel.y, width:r.width, height:r.height}];
          }))''')

    def stable_geometry(before, mobile=False):
        after = geometry()
        title, toolbar = after['.trend-heading > h2'], after['.trend-toolbar']
        assert abs(title['y'] - before['.trend-heading > h2']['y']) < 1, (before, after)
        natural_width = after['.trend-controls']['width']
        if compare.is_checked():
            natural_width += after['#comparisonControls']['width'] + 16
        fits = title['width'] + 18 + natural_width <= after['.trend-heading']['width'] + 1
        same_row = abs(toolbar['y'] - title['y']) < 1
        assert same_row == fits, (fits, before, after)
        if same_row:
            assert toolbar['x'] >= title['x'] + title['width']
        else:
            assert toolbar['y'] >= title['y'] + title['height'] + 11
            assert abs(toolbar['x'] - title['x']) < 1, 'entire toolbar should wrap below the title'
        assert abs(after['.trend-controls']['y'] - toolbar['y']) < 1
        assert after['#trendMetric']['height'] == 42
        assert after['#trendBucketTrigger']['height'] == 42
        if not mobile:
            rows = 2 if compare.is_checked() and after['#comparisonControls']['y'] > toolbar['y'] + 1 else 1
            assert toolbar['height'] == 42 * rows + 12 * (rows - 1), after

    def shared_ticks():
        page.wait_for_function("""async () => {
          const c = (await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#trendChart'));
          const options = c?.getOption().yAxis?.filter(Boolean);
          if (!options || options.length < 2) return false;
          const left = c.getModel().getComponent('yAxis', 0);
          const grid = left.axis.getTicksCoords({tickModel:left.getModel('splitLine')})
            .map(t => left.axis.toGlobalCoord(t.coord));
          return options.filter(a => a.splitLine.show).length === 1 && options.every((_, i) => {
            const axis = c.getModel().getComponent('yAxis', i).axis;
            const ticks = axis.getTicksCoords().map(t => axis.toGlobalCoord(t.coord));
            const labels = axis.getViewLabels().map(t => axis.toGlobalCoord(axis.dataToCoord(t.tick.value)));
            return ticks.length === grid.length && labels.length === grid.length
              && ticks.every((y, j) => Math.abs(y-grid[j]) < 0.5 && Math.abs(labels[j]-grid[j]) < 0.5);
          });
        }""")

    def hundred_tick(enabled=True):
        page.wait_for_function('''async enabled => {
          const c = (await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#trendChart'));
          const model = c?.getModel().getComponent('yAxis', 1);
          if (!model) return false;
          if (!enabled) return Array.isArray(model.get('axisLabel.customValues')) && Array.isArray(model.get('axisTick.customValues'));
          return model.axis.scale.getExtent()[1] >= 100
            && model.get('axisLabel.customValues')?.includes(100)
            && model.axis.getTicksCoords().some(t => t.tickValue === 100)
            && model.axis.getViewLabels().some(t => t.formattedLabel === '100%');
        }''', arg=enabled)

    def series(predict_on, estimate_on):
        page.wait_for_function('''async ([predict, estimate]) => {
          const c = (await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#trendChart'));
          const ids = c?.getOption().series.filter(Boolean).map(s => s?.id) || [];
          return ids.includes('comparison-quota:predict') === predict && ids.includes('estimated-quota') === estimate;
        }''', arg=[predict_on, estimate_on])
        shared_ticks()

    def custom_range(to):
        page.locator('#rangePickerTrigger').click()
        page.locator('[data-range-preset="custom"]').click()
        for prefix, timestamp in [('From', START - 60), ('To', to)]:
            values = page.evaluate('''at => {
              const d = new Date(at * 1000), pad = n => String(n).padStart(2, '0');
              return [d.getFullYear()+'-'+pad(d.getMonth()+1)+'-'+pad(d.getDate()),
                pad(d.getHours())+':'+pad(d.getMinutes())+':'+pad(d.getSeconds())];
            }''', timestamp)
            page.locator('#custom' + prefix + 'Date').fill(values[0])
            page.locator('#custom' + prefix + 'Time').fill(values[1])
        page.locator('#applyRange').click()
        expect(page.locator('#refreshButton')).to_be_enabled()
        expect(page.locator('#errorBanner')).to_be_hidden()

    def preset(name):
        page.locator('#rangePickerTrigger').click()
        page.locator(f'[data-range-preset="{name}"]').click()
        expect(page.locator('#refreshButton')).to_be_enabled()

    plain_geometry = geometry()
    stable_geometry(plain_geometry)
    compare.check()
    disable_history(page)
    expect(predict).to_be_checked()
    expect(estimate).to_be_checked()
    series(True, True)
    stable_geometry(plain_geometry)
    hundred_tick()
    for metric in ['totalRequests', 'realTotalTokens', 'totalCostUsd']:
        page.locator('#trendMetric').select_option(metric)
        shared_ticks()
    # Groups share a row when space permits; a wrapped comparison row remains right aligned.
    left = page.locator('.trend-controls').bounding_box()
    right = page.locator('#comparisonControls').bounding_box()
    toolbar = page.locator('.trend-toolbar').bounding_box()
    if abs(left['y'] - right['y']) < 2:
        assert right['x'] >= left['x'] + left['width'], (left, right)
    else:
        assert right['y'] >= left['y'] + left['height'] + 11, (left, right)
    assert abs(right['x'] + right['width'] - toolbar['x'] - toolbar['width']) < 2
    page.locator('.trend-toolbar').screenshot(path=str(artifacts / 'independent-controls-desktop.png'))
    estimate.uncheck()
    series(True, False)
    hundred_tick(False)
    predict.uncheck()
    series(False, False)
    predict.check()
    expect(estimate).to_be_checked()
    series(True, True)
    predict.uncheck()
    series(False, True)
    hundred_tick()
    # Historical range transition turns off Predict but retains a manually disabled estimate.
    predict.check()
    estimate.uncheck()
    custom_range(START + 3601)
    expect(predict).not_to_be_checked()
    expect(estimate).not_to_be_checked()
    series(False, False)
    predict.check()
    expect(estimate).to_be_checked()
    series(True, True)
    # Refresh preserves a deliberately enabled historical forecast.
    page.locator('#refreshButton').click()
    expect(page.locator('#refreshButton')).to_be_enabled()
    expect(predict).to_be_checked()
    series(True, True)
    # Re-entering Compare in history resets Predict even when shared state was on.
    compare.uncheck()
    compare.check()
    expect(predict).not_to_be_checked()
    expect(estimate).to_be_checked()
    series(False, True)
    estimate.uncheck()
    custom_range(NOW + 3600)
    expect(predict).not_to_be_checked()
    expect(estimate).not_to_be_checked()
    compare.uncheck()
    compare.check()
    expect(predict).to_be_checked()
    expect(estimate).to_be_checked()
    series(True, True)
    # Both shared Predict controls enable the independent estimate only on user activation.
    predict.uncheck()
    estimate.uncheck()
    page.locator('#quotaMetricFilter .quota-picker-trigger').click()
    page.locator('#quotaMetricFilter input[data-control="extra"]').first.check()
    page.keyboard.press('Escape')
    expect(predict).to_be_checked()
    expect(estimate).to_be_checked()
    series(True, True)
    estimate.uncheck()
    page.locator('#nodeFilter').select_option('node-c')
    expect(page.locator('#refreshButton')).to_be_enabled()
    expect(predict).to_be_checked()
    expect(estimate).not_to_be_checked()
    series(True, False)
    # Cancelling an estimate request must prevent its late response from restoring the line.
    held = []
    pattern = '**/v3/dashboard/quota/resets*'
    page.route(pattern, lambda route: held.append(route))
    estimate.check()
    page.wait_for_timeout(300)
    assert held, 'estimate should independently load reset history'
    hundred_tick()
    estimate.uncheck()
    for route in held:
        try:
            route.continue_()
        except Exception as error:
            # Chromium can dispose the route as soon as AbortController cancels it.
            assert 'closed' in str(error).lower() or 'invalid interception' in str(error).lower(), error
    page.unroute(pattern)
    series(True, False)
    expect(page.locator('#estimatedQuotaStatus')).to_be_hidden()
    # Failed estimates must retain 100%, without relying on any estimate series.
    page.route(pattern, lambda route: route.fulfill(status=503, body='unavailable'))
    estimate.check()
    expect(page.locator('#estimatedQuotaStatus')).to_contain_text('Unable to load')
    hundred_tick()
    series(True, False)
    estimate.uncheck()
    page.unroute(pattern)
    # Live presets must include now even when the displayed response predates the click.
    predict.uncheck()
    preset('24h')
    expect(predict).not_to_be_checked()
    expect(estimate).not_to_be_checked()
    page.wait_for_timeout(1100)
    compare.uncheck()
    compare.check()
    expect(predict).to_be_checked()
    expect(estimate).to_be_checked()
    series(True, True)
    estimate.uncheck()
    page.locator('#comparisonProvider').select_option('["node-b","shared"]')
    page.locator('#comparisonTier').select_option(TIER)
    expect(predict).not_to_be_checked()
    expect(estimate).not_to_be_checked()
    page.locator('#comparisonProvider').select_option(PROVIDER)
    page.locator('#comparisonTier').select_option(TIER)
    expect(predict).to_be_checked()
    expect(estimate).not_to_be_checked()
    estimate.check()
    series(True, True)
    for language in range(2):
        page.set_viewport_size({'width': 1440, 'height': 1000})
        compare.uncheck()
        plain_geometry = geometry()
        compare.check()
        series(True, True)
        stable_geometry(plain_geometry)
        hundred_tick()
        provider_label, tier_label = ('Quota Provider', 'Quota window') if language == 0 else ('额度提供方', '额度周期')
        expect(page.get_by_role('combobox', name=provider_label, exact=True)).to_have_attribute('title', provider_label)
        expect(page.get_by_role('combobox', name=tier_label, exact=True)).to_have_attribute('title', tier_label)
        page.locator('.trend-heading').screenshot(path=str(artifacts / f'inline-heading-desktop-{language}.png'))
        page.locator('#trendChart').locator('..').screenshot(path=str(artifacts / f'hundred-percent-desktop-{language}.png'))
        for width in [1100, 900]:
            page.set_viewport_size({'width': width, 'height': 900})
            stable_geometry(geometry(), mobile=True)
            assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
        page.set_viewport_size({'width': 390, 'height': 844})
        compare.uncheck()
        plain_geometry = geometry()
        compare.check()
        series(True, True)
        stable_geometry(plain_geometry, mobile=True)
        hundred_tick()
        assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
        page.locator('.trend-heading').screenshot(path=str(artifacts / f'inline-heading-mobile-{language}.png'))
        page.locator('#languageToggle').click()
    # Exercise the KPI formatter with controlled API ratios, including padding and rounding.
    ratio = 0
    def cache_ratio(route):
        response = route.fetch()
        data = response.json()
        data['summary']['cacheHitRate'] = ratio
        route.fulfill(response=response, json=data)
    page.route('**/v3/dashboard/overview?*', cache_ratio)
    for ratio, label in [(0.123456, '12.35%'), (0.1, '10.00%'), (1, '100.00%')]:
        page.locator('#refreshButton').click()
        expect(page.locator('#refreshButton')).to_be_enabled()
        expect(page.locator('#kpiCacheRate')).to_have_text(label)
    page.unroute('**/v3/dashboard/overview?*')
    assert not errors, errors
    print('PASS: independent estimate/Predict, historical/live/custom transitions, shared controls, stale cancellation, formatting; 100% tick during load/failure; shared axis/grid pixel positions; inline heading and whole-toolbar wrapping in both languages')


def main():
    with tempfile.TemporaryDirectory(prefix="quota-comparison-") as directory:
        work = Path(directory)
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        origin = f"http://127.0.0.1:{port}"
        database = work / "telemetry.db"
        settings = {"version": 1, "quotaDefaults": {"providers": None}, "quotaProviderAliases": [], "dashboardDefaults": {
            "rangePreset": "last-reset", "lastReset": {"nodeId": "node-a", "providerId": "shared", "metricKey": "5h", "metricKind": "utilizationPercent", "unit": "%"}}}
        (work / "settings.json").write_text(json.dumps(settings))
        environment = dict(os.environ, TELEMETRY_DB=str(database), TELEMETRY_LISTEN=f"127.0.0.1:{port}")
        environment.pop("ADMIN_PASSWORD", None)
        with (work / "server.log").open("w") as log:
            server = subprocess.Popen([str(ROOT / "target/release/telemetry-server")], env=environment, stdout=log, stderr=log)
            try:
                http = build_opener(ProxyHandler({}))
                for _ in range(100):
                    if server.poll() is not None:
                        raise RuntimeError("temporary server failed to start: " + (work / "server.log").read_text())
                    try:
                        with http.open(origin + "/healthz", timeout=1):
                            break
                    except OSError:
                        time.sleep(.1)
                else:
                    raise RuntimeError("temporary server did not become ready")
                seed(database)
                artifacts = ROOT / "artifacts/quota-comparison-tests"
                artifacts.mkdir(parents=True, exist_ok=True)
                with sync_playwright() as playwright:
                    browser = playwright.chromium.launch(headless=True)
                    try:
                        page = browser.new_page(locale="en-US", viewport={"width": 1440, "height": 1000})
                        check(page, origin, artifacts)
                        # Other browser suites reuse main() with their own check function.
                        if check.__module__ == __name__:
                            controls_page = browser.new_page(locale="en-US", viewport={"width": 1440, "height": 1000})
                            check_controls(controls_page, origin, artifacts)
                    finally:
                        browser.close()
            finally:
                server.terminate()
                server.wait(timeout=10)


if __name__ == "__main__":
    main()
