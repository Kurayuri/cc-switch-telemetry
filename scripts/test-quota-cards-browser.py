#!/usr/bin/env python3
"""Quota card layout and data regression on an isolated Dashboard."""
import copy
import importlib.util
from pathlib import Path
from playwright.sync_api import expect

spec = importlib.util.spec_from_file_location('comparison_fixture', Path(__file__).with_name('test-quota-comparison-browser.py'))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def check(page, origin, artifacts):
    errors = []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.goto(origin + '/dashboard/')
    expect(page.locator('#refreshButton')).to_be_enabled()
    expect(page.locator('.quota-card')).to_have_count(3)
    expect(page.locator('.quota-exhaustion[data-status="estimated"]')).to_have_count(6)
    # Current fixture values and all metadata remain present without enabling Predict.
    expect(page.locator('.quota-metric-value').first).to_have_text('60.0%')
    assert page.locator('#quotaMetricFilter input[data-control="extra"]:checked').count() == 0
    expect(page.locator('.quota-meter')).to_have_count(6)
    assert page.locator('.quota-exhaustion-early').count() == 6
    expect(page.locator('.quota-card-footer .quota-detail')).to_have_count(6)
    before = page.locator('.quota-card').first.inner_text()
    assert '$3.0500' in before, before  # Existing page-total / utilization formula.
    def assert_visible_fields():
        result = page.locator('.quota-cards').evaluate('''host => {
          const fields=[...host.querySelectorAll('.quota-card-heading strong,.quota-card-heading small,.quota-detail strong,.quota-metric-value')];
          return fields.filter(e=>e.scrollWidth>e.clientWidth+1 || e.scrollHeight>e.clientHeight+1).map(e=>({text:e.textContent,sw:e.scrollWidth,cw:e.clientWidth,sh:e.scrollHeight,ch:e.clientHeight}));
        }''')
        assert not result, result
        assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
    heights = []
    for lang in ['en-US', 'zh-CN']:
        if page.locator('html').get_attribute('lang') != lang:
            page.locator('#languageToggle').click()
        for theme in ['dark', 'light']:
            if page.locator('html').get_attribute('data-theme') != theme:
                page.locator('#themeToggle').click()
            for width in [1440, 900, 390, 320]:
                page.set_viewport_size({'width': width, 'height': 1000})
                card = page.locator('.quota-card').first
                card.scroll_into_view_if_needed()
                groups = card.locator('.quota-metric-group')
                a, b = groups.nth(0).bounding_box(), groups.nth(1).bounding_box()
                if width >= 390:
                    assert abs(a['y'] - b['y']) < 1 and b['x'] > a['x'], (width, a, b)
                else:
                    assert b['y'] > a['y'], (width, a, b)
                assert_visible_fields()
                if width == 1440:
                    height = card.bounding_box()['height']
                    heights.append((lang, theme, height))
                    assert 190 <= height <= 220, (lang, theme, height)
                card.screenshot(path=str(artifacts / f'quota-kpi-{lang}-{theme}-{width}.png'))
    print('Normal dual-cycle heights:', heights)
    # No hover movement or appearance change; all fields remain outside disclosures.
    page.set_viewport_size({'width': 1440, 'height': 1000})
    card = page.locator('.quota-card').first
    card.scroll_into_view_if_needed(); page.mouse.move(0, 0)
    appearance = 'e=>({border:getComputedStyle(e).borderColor,transform:getComputedStyle(e).transform,bg:getComputedStyle(e).backgroundImage})'
    before_hover = card.evaluate(appearance)
    card.hover(); page.wait_for_timeout(200)
    assert card.evaluate(appearance) == before_hover

    def cases(route):
        response = route.fetch()
        data = response.json()
        if not data.get('providers'):
            route.fulfill(response=response)
            return
        base = data['providers'][0]
        def provider(name):
            item = copy.deepcopy(base)
            item['providerId'] = name
            item['providerName'] = name
            return item
        single = provider('Single')
        single['current'] = single['current'][:1]
        single['series'] = single['series'][:1]
        multi = provider('Multi')
        balance = {'key': 'credits', 'label': 'Credit balance', 'kind': 'balance', 'unit': 'USD', 'remaining': 123.45, 'used': None, 'total': None, 'utilizationPercent': None, 'resetsAt': None, 'sampledAt': fixture.NOW}
        multi['current'].append(balance)
        empty = provider('Long-provider-' + 'name-' * 12)
        empty['nodeName'] = 'Long-node-' + 'identity-' * 10
        empty['status'] = 'query_failed'
        empty['current'] = []; empty['series'] = []
        zero = provider('No-estimate')
        zero['current'] = zero['current'][:1]
        zero['current'][0]['utilizationPercent'] = 0
        zero['series'] = []
        data['providers'] = [base, single, multi, empty, zero]
        route.fulfill(response=response, json=data)
    page.route('**/v3/dashboard/quota?*', cases)
    if page.locator('html').get_attribute('lang') != 'en-US': page.locator('#languageToggle').click()
    page.locator('#refreshButton').click(); expect(page.locator('#refreshButton')).to_be_enabled()
    expect(page.locator('.quota-card')).to_have_count(5)
    cards = page.locator('.quota-card')
    expect(cards.nth(1).locator('.quota-metric-group')).to_have_count(1)
    expect(cards.nth(2).locator('.quota-metric-group')).to_have_count(3)
    expect(cards.nth(2).locator('.quota-meter')).to_have_count(2)
    expect(cards.nth(2).locator('.quota-metric-amount .quota-metric-value')).to_contain_text('USD')
    expect(cards.nth(3).locator('.quota-status')).to_contain_text('failed')
    expect(cards.nth(3).locator('.quota-metric-group')).to_have_count(0)
    expect(cards.nth(3)).to_contain_text('No successful')
    expect(cards.nth(4).locator('.quota-exhaustion')).to_contain_text('—')
    for width in [1440, 390]:
        page.set_viewport_size({'width': width, 'height': 1000})
        assert_visible_fields()
        assert cards.nth(3).locator('.quota-status').bounding_box()['height'] < 40
        if width == 1440:
            assert cards.nth(4).bounding_box()['y'] > cards.nth(0).bounding_box()['y']
        cards.nth(2).screenshot(path=str(artifacts / f'quota-kpi-multi-{width}.png'))
        cards.nth(3).screenshot(path=str(artifacts / f'quota-kpi-error-{width}.png'))
    assert not errors, errors
    print('PASS: dual/single/multiple metrics, percentage meters, balance units, empty/error/long identity, early estimates independent of Predict, all metadata, responsive themes/locales')


fixture.check = check
if __name__ == '__main__':
    fixture.main()
