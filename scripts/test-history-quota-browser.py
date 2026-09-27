#!/usr/bin/env python3
"""Historical references through real endpoints, isolated database and browser."""
import importlib.util
import sqlite3
from pathlib import Path
from playwright.sync_api import expect

spec = importlib.util.spec_from_file_location('comparison_fixture', Path(__file__).with_name('test-quota-comparison-browser.py'))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
base_seed = fixture.seed


def seed(path):
    base_seed(path)
    with sqlite3.connect(path) as db:
        for index, cost in [(2, 40), (1, 20)]:
            start = fixture.START - index * 18000
            end = start + 18000
            values = [20, 100 if index == 2 else 30, 40, 50, 0]
            for step, percent in enumerate(values):
                at = start + step * 60
                oid = f'history-{index}-{step}'
                db.execute("INSERT INTO quota_observations VALUES ('node-a',?,'test','codex','shared',?,?)", (oid, at, at))
                db.execute("INSERT INTO quota_metrics VALUES ('node-a',?,'5h','5h','utilizationPercent',?,NULL,NULL,NULL,'%',?)", (oid, percent, end))
            for at, amount in [(start + 10, cost), (start + 210, 999)]:
                eid = f'history-cost-{at}'
                db.execute("""INSERT INTO usage_events
                    (event_id,node_id,request_id,created_at,app_type,provider_id,model,
                    input_tokens,output_tokens,cache_read_tokens,cache_creation_tokens,
                    total_cost_usd,latency_ms,status_code,is_streaming,data_source,received_at)
                    VALUES (?,'node-a',?,?,'codex','shared','test-model',100,50,0,0,?,10,200,0,'test',?)""", (eid, eid, at, str(amount), at))
        db.execute("""INSERT INTO usage_cache_partitions(node_id,hour_start,state,updated_at)
            SELECT node_id,created_at-(created_at%3600),'dirty',0 FROM usage_events GROUP BY node_id,created_at-(created_at%3600)
            ON CONFLICT(node_id,hour_start) DO UPDATE SET state='dirty'""")


def check(page, origin, artifacts):
    errors = []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.goto(origin + '/dashboard/')
    expect(page.locator('#refreshButton')).to_be_enabled()
    page.locator('#nodeFilter').select_option('node-a')
    expect(page.locator('#refreshButton')).to_be_enabled()
    page.locator('#trendCompare').check()
    expect(page.locator('#comparisonProvider')).to_have_value(fixture.PROVIDER)
    expect(page.locator('#comparisonTier')).to_have_value(fixture.TIER)
    chart = "(await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#trendChart'))"
    def reference(amount):
        page.wait_for_function(f"async amount => {{const o=({chart})?.getOption(); return o?.series.some(s=>s?.id==='history-quota' && Math.abs(s.data[0].reference.amount-amount)<1e-9)}}", arg=amount)
    def absent():
        page.wait_for_function(f"async () => !({chart}).getOption().series.some(s=>s?.id==='history-quota')")
    def apply(mode, amount=None, cycle=None):
        page.locator('#historyReferenceEdit').click()
        page.locator('#historyReferenceSource').select_option(mode)
        if amount is not None:
            page.locator('#historyReferenceAmount').fill(str(amount))
        if cycle is not None:
            expect(page.locator('#historyReferenceCycle option')).to_have_count(3)
            page.locator('#historyReferenceCycle').select_option(str(cycle))
        expect(page.locator('#historyReferenceApply')).to_be_enabled()
        page.locator('#historyReferenceApply').click()

    expect(page.locator('#comparisonHistory')).to_have_count(0)
    reference(80)  # Default history is applied without opening the dialog.
    page.locator('#historyReferenceEdit').click()
    expect(page.locator('#historyReferenceDialog')).to_be_visible()
    expect(page.locator('#historyReferenceSource')).to_have_value('full')
    expect(page.locator('#historyReferencePreview')).to_contain_text('$80.0000')
    page.locator('#historyReferenceApply').click()
    reference(80)
    expect(page.locator('#comparisonPredict')).to_be_checked()
    expect(page.locator('#comparisonEstimatedQuota')).to_be_checked()
    page.locator('#historyReferenceEdit').click()
    page.locator('#historyReferenceSource').select_option('manual')
    page.locator('#historyReferenceAmount').fill('0')
    expect(page.locator('#historyReferenceApply')).to_be_disabled()
    page.locator('#historyReferenceAmount').fill('200')
    expect(page.locator('#historyReferenceApply')).to_be_enabled()
    page.locator('#historyReferenceCancel').click()
    reference(80)
    apply('previous'); reference(40)
    page.locator('#rangePickerTrigger').click()
    page.locator('[data-range-preset="last-reset"]').click()
    expect(page.locator('#applyRange')).to_be_enabled()
    page.locator('#lastResetProvider').select_option(fixture.PROVIDER)
    page.locator('#lastResetTier').select_option(fixture.TIER)
    page.locator('#resetCycleSelect').select_option(str(fixture.START))
    page.locator('#applyRange').click()
    expect(page.locator('#refreshButton')).to_be_enabled()
    reference(80)  # Previous is relative to this viewed cycle, not wall-clock now.
    page.locator('#rangePickerTrigger').click()
    page.locator('[data-range-preset="last-reset"]').click()
    page.locator('#applyRange').click()
    expect(page.locator('#refreshButton')).to_be_enabled()
    reference(40)
    apply('cycle', cycle=fixture.START - 18000); reference(80)
    apply('manual', amount=80); reference(80)
    page.locator('#comparisonEstimatedQuota').uncheck()
    page.locator('#comparisonPredict').uncheck()
    for metric in ['totalCostUsd', 'realTotalTokens', 'totalRequests']:
        page.locator('#trendMetric').select_option(metric)
        for cumulative in [False, True]:
            page.locator('#trendCumulative').set_checked(cumulative)
            page.evaluate('() => new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))')
            result = page.evaluate(f'''async () => {{
              const c=({chart}), o=c.getOption(), ref=o.series.find(s=>s?.id==='history-quota');
              const axes=o.yAxis.map((_,i)=>c.getModel().getComponent('yAxis',i).axis);
              const y=(i,v)=>axes[i].toGlobalCoord(axes[i].dataToCoord(v));
              const ticks=axes.map(a=>a.getTicksCoords().map(t=>a.toGlobalCoord(t.coord)));
              return {{delta:Math.abs(y(ref.yAxisIndex,80)-y(1,100)), ticks,
                dollarIndex:ref.yAxisIndex, count:axes.length}};
            }}''')
            assert result['delta'] < .5, result
            assert result['count'] == (2 if metric == 'totalCostUsd' else 3), result
            for ticks in result['ticks'][1:]:
                assert len(ticks) == len(result['ticks'][0]), result
                assert all(abs(a-b)<.5 for a,b in zip(ticks,result['ticks'][0])), result
    for width in [1440, 390]:
        page.set_viewport_size({'width':width,'height':1000})
        page.locator('#trendChart').scroll_into_view_if_needed()
        page.screenshot(path=str(artifacts / f'history-reference-{width}.png'))
        assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
        page.locator('#historyReferenceEdit').click()
        expect(page.locator('#historyReferenceDialog')).to_be_visible()
        bounds = page.locator('#historyReferenceDialog').bounding_box()
        assert 0 <= bounds['x'] and bounds['x'] + bounds['width'] <= width, bounds
        page.screenshot(path=str(artifacts / f'history-dialog-{width}.png'))
        page.keyboard.press('Escape')
        expect(page.locator('#historyReferenceDialog')).to_be_hidden()
    page.set_viewport_size({'width':1440,'height':1000})
    page.locator('#historyReferenceEdit').click()
    page.locator('#historyReferenceSource').select_option('none')
    expect(page.locator('#historyReferenceApply')).to_be_enabled()
    page.locator('#historyReferenceCancel').click(); reference(80)
    apply('none'); absent()
    expect(page.locator('#historyReferenceEdit')).to_contain_text('None')
    expect(page.locator('#comparisonPredict')).not_to_be_checked()
    expect(page.locator('#comparisonEstimatedQuota')).not_to_be_checked()
    page.locator('#trendCompare').uncheck()
    page.locator('#trendCompare').check()
    expect(page.locator('#historyReferenceEdit')).to_contain_text('None')
    absent()
    # Explicit cycles are identity scoped; changing tier cannot reuse an old amount.
    page.locator('#historyReferenceEdit').click()
    page.locator('#historyReferenceSource').select_option('cycle')
    expect(page.locator('#historyReferenceCycle option')).to_have_count(3)
    page.locator('#historyReferenceCycle').select_option(str(fixture.START - 18000))
    expect(page.locator('#historyReferenceApply')).to_be_enabled()
    page.locator('#historyReferenceApply').click(); reference(80)
    page.locator('#comparisonTier').select_option('["7d","utilizationPercent","%"]')
    absent()
    expect(page.locator('#historyReferenceEdit')).to_contain_text('—')
    page.locator('#historyReferenceEdit').click()
    expect(page.locator('#historyReferencePreview')).to_contain_text('Choose a historical cycle')
    expect(page.locator('#historyReferenceApply')).to_be_disabled()
    page.locator('#historyReferenceSource').select_option('none')
    expect(page.locator('#historyReferenceApply')).to_be_enabled()
    page.locator('#historyReferenceApply').click(); absent()
    assert not errors, errors
    print('PASS: default History without checkbox, None/apply/cancel, preserved opt-out; real historical cost/sample pairing, full/previous/cycle/manual, cancel/apply, independent controls, all metric grid pixels, mobile dialog, identity invalidation')


fixture.seed = seed
fixture.check = check
if __name__ == '__main__':
    fixture.main()
