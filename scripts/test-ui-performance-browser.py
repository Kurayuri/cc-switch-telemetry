#!/usr/bin/env python3
"""Regression for independent refresh, ordinary trend updates and stale responses."""
import importlib.util
from pathlib import Path
from playwright.sync_api import expect

spec=importlib.util.spec_from_file_location('fixture',Path(__file__).with_name('test-quota-comparison-browser.py'))
fixture=importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)

def check(page,origin,artifacts):
    errors=[]; requests=[]
    page.on('pageerror',lambda error:errors.append(str(error)))
    page.on('request',lambda request:requests.append(request.url))
    page.goto(origin+'/dashboard/')
    chart="(await import('/dashboard/vendor/echarts.esm.min.mjs')).getInstanceByDom(document.querySelector('#trendChart'))"
    page.wait_for_function(f"async () => Boolean(({chart})?.getOption()?.series?.length)")
    expect(page.locator('#refreshButton')).to_be_enabled()
    page.locator('#trendMetric').select_option('totalRequests')
    page.evaluate('() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))')
    # Overview and ordinary trend must update while the unrelated daily query is held.
    held=[]
    page.route('**/v3/dashboard/daily?*',lambda route:held.append(route))
    requests.clear()
    page.locator('#nodeFilter').select_option('node-c')
    page.wait_for_function(f"async () => ({chart}).getOption().series[0].data.reduce((n,p)=>n+p.value[1],0)===61")
    page.wait_for_timeout(50)
    assert held and page.locator('#refreshButton').is_disabled()
    assert not any('/quota?' in url or '/settings' in url for url in requests),requests
    for route in held:route.continue_()
    page.unroute('**/v3/dashboard/daily?*')
    expect(page.locator('#refreshButton')).to_be_enabled()
    # Time-range changes reuse the unchanged full-year daily resource.
    old=page.evaluate(f"async () => ({chart}).getOption().xAxis[0].min")
    held=[];requests.clear()
    page.route('**/v3/dashboard/quota?*',lambda route:held.append(route))
    page.locator('#rangePickerTrigger').click()
    page.locator('[data-range-preset="7d"]').click()
    page.wait_for_function(f"async old => ({chart}).getOption().xAxis[0].min < old",arg=old)
    page.wait_for_timeout(50)
    assert held and page.locator('#refreshButton').is_disabled()
    assert not any('/daily?' in url for url in requests),requests
    for route in held:route.continue_()
    page.unroute('**/v3/dashboard/quota?*')
    expect(page.locator('#refreshButton')).to_be_enabled()
    # Reversed completion order cannot replace the final selection.
    held=[]
    page.route('**/v3/dashboard/overview?*',lambda route:held.append(route))
    page.locator('#nodeFilter').select_option('node-a')
    page.wait_for_timeout(100)
    page.locator('#nodeFilter').select_option('node-b')
    page.wait_for_timeout(100)
    assert len(held)==2
    held[1].continue_()
    expect(page.locator('#refreshButton')).to_be_enabled()
    held[0].continue_()
    page.unroute('**/v3/dashboard/overview?*')
    page.wait_for_timeout(100)
    expect(page.locator('#nodeFilter')).to_have_value('node-b')
    assert page.locator('#eventRows tr').count()>0
    assert all('node-b' in row for row in page.locator('#eventRows tr').all_text_contents())
    assert not errors,errors
    page.screenshot(path=str(artifacts/'independent-refresh.png'))
    print('PASS: ordinary trend updates before unrelated queries; filter/range request scope; latest selection wins')

fixture.check=check
if __name__=='__main__':fixture.main()
