#!/usr/bin/env python3
"""Isolated settings/weighted-cost and retained-prediction browser regression."""
import importlib.util
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlencode
from urllib.request import ProxyHandler, build_opener
from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('comparison_fixture', ROOT/'scripts/test-quota-comparison-browser.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
fixture.START -= 300
fixture.RESET -= 300
CATALOG = {'fixture': {'models': {'test-model': {'cost': {'input': 2, 'output': 8, 'cache_read': .5, 'cache_write': 4}}}}}

class Prices(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps(CATALOG).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *_):
        pass


def check(browser, origin, database, artifacts):
    context = browser.new_context(locale='en-US', viewport={'width': 1440, 'height': 1000})
    admin = context.new_page()
    errors = []
    admin.on('pageerror', lambda error: errors.append(str(error)))
    assert admin.request.get(origin+'/admin/api/model-pricing?model=test-model').status == 401
    assert admin.request.post(origin+'/admin/login', data={'password': 'isolated-test-password'}).ok
    admin.goto(origin+'/admin/')
    expect(admin.locator('#saveSettings')).to_be_enabled()
    row = admin.locator('.settings-model-billing-row').first
    row.locator('input[type=text]').fill('test-model')
    expect(row.locator('.settings-billing-mode option')).to_have_text(['整体', '分项'])
    row.locator('.settings-billing-mode').select_option('components')
    expect(row.locator('.settings-billing-pricing')).to_contain_text('fixture/test-model')
    for key, value in [('freshMultiplier','2'),('creationMultiplier','3'),('readMultiplier','4'),('outputMultiplier','2')]:
        row.locator(f'[data-factor={key}]').fill(value)
    with admin.expect_response(lambda r: r.url.endswith('/admin/api/settings') and r.request.method == 'PUT') as saved:
        admin.locator('#saveSettings').click()
    assert saved.value.ok, saved.value.text()
    settings = saved.value.json()
    entry = settings['dashboardDefaults']['modelBillingMultipliers'][0]
    assert entry['mode'] == 'components' and entry['referencePricing']['fresh'] == 2
    admin.reload()
    expect(admin.locator('.settings-billing-mode').first).to_have_value('components')
    expect(admin.locator('[data-factor=readMultiplier]').first).to_have_value('4')
    expect(admin.locator('[data-factor=outputMultiplier]').first).to_have_value('2')
    admin.locator('#settingsModelBillingMultipliers').scroll_into_view_if_needed()
    admin.locator('.settings-billing').screenshot(path=str(artifacts/'billing-desktop.png'))
    admin.set_viewport_size({'width':390,'height':844})
    admin.locator('.settings-billing').screenshot(path=str(artifacts/'billing-mobile.png'))
    assert admin.evaluate('document.documentElement.scrollWidth <= innerWidth')

    params = dict(from_=fixture.START, to=fixture.NOW+1, bucket='1m', node_id='node-c')
    query = urlencode({('from' if k=='from_' else k): v for k,v in params.items()})
    def overview():
        r = admin.request.get(origin+'/v3/dashboard/overview?'+query)
        assert r.ok, r.text()
        return r.json()
    weighted = overview()
    original = .61
    assert abs(weighted['summary']['totalCostUsd'] - original*1500/695) < 1e-9
    tokens = weighted['summary']['realTotalTokens']
    for mode, fields, expected in [
        ('overall', {'multiplier':2},original*2),
        ('input', {'inputMultiplier':2},original*990/695),
        ('components', {'freshMultiplier':1,'creationMultiplier':1,'readMultiplier':1,'outputMultiplier':1},original),
    ]:
        payload = json.loads(json.dumps(settings))
        value = payload['dashboardDefaults']['modelBillingMultipliers'][0]
        value.update(mode=mode, **fields)
        response = admin.request.put(origin+'/admin/api/settings', data=payload)
        assert response.ok, response.text()
        if mode == 'input':  # Legacy API payloads migrate without changing the fee.
            migrated = response.json()['dashboardDefaults']['modelBillingMultipliers'][0]
            assert migrated['mode'] == 'components' and 'inputMultiplier' not in migrated
            assert [migrated[k] for k in ['freshMultiplier','creationMultiplier','readMultiplier','outputMultiplier']] == [2,2,2,1]
        result = overview()
        assert abs(result['summary']['totalCostUsd']-expected) < 1e-9, (mode,result['summary'])
        assert result['summary']['realTotalTokens'] == tokens
    # Missing creation price preserves each original amount and exposes counts.
    missing = json.loads(json.dumps(settings))
    missing['dashboardDefaults']['modelBillingMultipliers'][0]['referencePricing']['creation'] = None
    assert admin.request.put(origin+'/admin/api/settings', data=missing).ok
    result = overview()
    assert abs(result['summary']['totalCostUsd']-original)<1e-9
    assert result['summary']['unadjustedCostRequests']==61
    unresolved = json.loads(json.dumps(settings))
    unresolved['dashboardDefaults']['modelBillingMultipliers'][0].update(model='unknown-model',referencePricing=None)
    response = admin.request.put(origin+'/admin/api/settings', data=unresolved)
    assert response.ok and response.json()['dashboardDefaults']['modelBillingMultipliers'][0]['referencePricing'] is None
    assert admin.request.put(origin+'/admin/api/settings', data=settings).ok

    page = context.new_page()
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.goto(origin+'/dashboard/')
    expect(page.locator('#refreshButton')).to_be_enabled()
    page.locator('#nodeFilter').select_option('node-c')
    expect(page.locator('#refreshButton')).to_be_enabled()
    expect(page.locator('#kpiCostAdjustment')).to_be_hidden()
    expect(page.locator('#kpiCost')).to_contain_text('1.3165')
    with page.expect_request_finished(lambda request: '/v3/dashboard/overview?' in request.url and 'bucket=quota-auto' in request.url):
        page.locator('#trendCompare').check()
    page.locator('#comparisonPredict').check()
    chart = "(await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#trendChart'))"
    ready = """async () => {
      const c = (await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#trendChart'));
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      return c?.getOption().series.find(s=>s?.id==='estimated-quota')?.data || false;
    }"""
    page.wait_for_function(ready)
    expect(page.locator('#estimatedQuotaStatus')).to_be_hidden()
    before = page.wait_for_function(ready).json_value()
    expected_cycle_cost = 0
    for item in before:
        if item['value'][1] is None: continue
        p=item['estimate']
        expected_cycle_cost=.01*1500/695*sum(fixture.START+step*60+10<p['at'] for step in range(61))
        assert abs(p['cost']-expected_cycle_cost)<1e-9
    # Observe every setOption to catch removal/re-addition even within one frame.
    page.evaluate(f"""async () => {{
      const c = ({chart}); const set = c.setOption.bind(c);
      window.refreshRenders=[];
      c.setOption=(option,...args)=>{{
        window.refreshRenders.push({{present:option.series.some(s=>s?.id==='estimated-quota'), animation:option.animation,
          seriesAnimation:option.series.find(s=>s?.id==='estimated-quota')?.animation}});
        return set(option,...args);
      }};
      c.dispatchAction({{type:'legendUnSelect',name:'Est. quota'}});
    }}""")
    requests=[]
    page.on('request',lambda req: requests.append(req.url))
    for manual in [False, False, True]:
        held=[]
        page.route('**/v3/dashboard/quota?*', lambda route: held.append(route))
        if manual: page.locator('#refreshButton').click()
        else: page.evaluate("document.dispatchEvent(new Event('visibilitychange'))")
        page.wait_for_timeout(250)
        assert held
        assert page.evaluate(f"async () => ({chart}).getOption().series.find(s=>s?.id==='estimated-quota').data") == before
        for route in held: route.continue_()
        page.unroute('**/v3/dashboard/quota?*')
        expect(page.locator('#refreshButton')).to_be_enabled()
        page.wait_for_timeout(400)
        page.wait_for_function(ready)
        option=page.evaluate(f"async () => ({{legend:({chart}).getOption().legend[0].selected, data:({chart}).getOption().series.find(s=>s?.id==='estimated-quota').data}})")
        assert option['legend'].get('Est. quota') is False
        before=option['data']
        if not manual:
            assert not any('/quota/resets' in url for url in requests), requests
        requests.clear()
    renders=page.evaluate('window.refreshRenders')
    assert renders and all(r['present'] and r['animation'] is False and r['seriesAnimation'] is False for r in renders), renders
    # Failed revalidation preserves the last complete line and reports failure.
    page.route('**/v3/dashboard/quota/resets', lambda route: route.fulfill(status=503,body='unavailable'))
    page.locator('#refreshButton').click()
    expect(page.locator('#refreshButton')).to_be_enabled()
    expect(page.locator('#estimatedQuotaStatus')).to_contain_text('Unable to load')
    assert page.evaluate(f"async () => ({chart}).getOption().series.find(s=>s?.id==='estimated-quota').data") == before
    page.unroute('**/v3/dashboard/quota/resets')
    page.locator('#refreshButton').click()
    expect(page.locator('#refreshButton')).to_be_enabled()
    expect(page.locator('#estimatedQuotaStatus')).to_be_hidden()
    # Raw data never changes as a consequence of display settings.
    with sqlite3.connect(database) as db:
        assert abs(db.execute("SELECT SUM(CAST(total_cost_usd AS REAL)) FROM usage_events WHERE node_id='node-c'").fetchone()[0]-.61)<1e-9
    assert not errors, errors
    print(f'PASS: persisted billing modes/snapshots, weighted APIs and quota curve, missing-price fallback, preserved raw data; {len(renders)} refresh renders retained the line without animation')
    context.close()


def main():
    with tempfile.TemporaryDirectory(prefix='billing-refresh-') as temp:
        work=Path(temp)
        prices=ThreadingHTTPServer(('127.0.0.1',0),Prices)
        thread=threading.Thread(target=prices.serve_forever,daemon=True);thread.start()
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1',0));port=reservation.getsockname()[1]
        origin=f'http://127.0.0.1:{port}'
        database=work/'telemetry.db'
        (work/'settings.json').write_text(json.dumps({'version':1,'quotaDefaults':{'providers':None},'quotaProviderAliases':[],
            'dashboardDefaults':{'rangePreset':'last-reset','lastReset':{'nodeId':'node-a','providerId':'shared','metricKey':'5h','metricKind':'utilizationPercent','unit':'%'}}}))
        env=dict(os.environ,TELEMETRY_DB=str(database),TELEMETRY_LISTEN=f'127.0.0.1:{port}',ADMIN_PASSWORD='isolated-test-password',
            TELEMETRY_MODELS_DEV_URL=f'http://127.0.0.1:{prices.server_port}/api.json')
        with (work/'server.log').open('w') as log:
            server=subprocess.Popen([str(ROOT/'target/release/telemetry-server')],env=env,stdout=log,stderr=log)
            try:
                http=build_opener(ProxyHandler({}))
                for _ in range(100):
                    if server.poll() is not None: raise RuntimeError((work/'server.log').read_text())
                    try:
                        http.open(origin+'/healthz',timeout=1).close();break
                    except OSError: time.sleep(.1)
                fixture.seed(database)
                with sqlite3.connect(database) as db:
                    db.execute("UPDATE usage_events SET input_tokens=150,cache_creation_tokens=20,cache_read_tokens=30,input_token_semantics=1 WHERE node_id='node-c'")
                    db.execute("INSERT INTO usage_cache_partitions(node_id,hour_start,state,updated_at) SELECT node_id,created_at-(created_at%3600),'dirty',0 FROM usage_events GROUP BY node_id,created_at-(created_at%3600) ON CONFLICT(node_id,hour_start) DO UPDATE SET state='dirty'")
                artifacts=ROOT/'artifacts/billing-refresh-tests';artifacts.mkdir(parents=True,exist_ok=True)
                with sync_playwright() as playwright:
                    browser=playwright.chromium.launch(headless=True)
                    try: check(browser,origin,database,artifacts)
                    finally: browser.close()
            finally:
                server.terminate();server.wait(timeout=10)
                prices.shutdown();prices.server_close()

if __name__=='__main__': main()
