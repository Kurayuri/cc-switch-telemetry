#!/usr/bin/env python3
"""Isolated Fast request filters, metadata and Dashboard appearance regression."""
import importlib.util
import sqlite3
from pathlib import Path
from urllib.parse import urlparse, parse_qs
from playwright.sync_api import expect

spec = importlib.util.spec_from_file_location('comparison_fixture', Path(__file__).with_name('test-quota-comparison-browser.py'))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
original_seed = fixture.seed

def seed(path):
    original_seed(path)
    with sqlite3.connect(path) as db:
        db.execute("UPDATE usage_events SET model='gpt-5.5',service_tier='fast',service_tier_source='request',reasoning_effort='high',service_tier_pricing_version=2 WHERE node_id='node-a'")
        db.execute("UPDATE usage_events SET model='gpt-5.5',service_tier='priority',service_tier_source='response',reasoning_effort='low',service_tier_pricing_version=2 WHERE node_id='node-b'")
        db.execute("UPDATE quota_provider_states SET status='command_failed',diagnostic_code='cli_schema_incompatible' WHERE node_id='node-c'")

def check(page, origin, artifacts):
    errors, calls = [], []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.on('request', lambda request: calls.append(request.url))
    page.goto(origin + '/dashboard/')
    expect(page.locator('#refreshButton')).to_be_enabled()
    expect(page.locator('#eventRows tr')).to_have_count(50)
    expect(page.locator('#errorBanner')).to_be_hidden()
    expect(page.locator('.quota-status').last).to_have_text('CLI incompatible')
    assert 'database version' in page.locator('.quota-status').last.get_attribute('title')
    expect(page.locator('#kpiRequests')).to_have_text('183')
    totals = [page.locator('#kpiRequests').inner_text(), page.locator('#kpiCost').inner_text()]
    start = len(calls)
    with page.expect_response(lambda r: '/v3/dashboard/events?' in r.url and 'service_tier=fast' in r.url):
        page.locator('#eventTierFilter').select_option('fast')
    expect(page.locator('#eventRows tr')).to_have_count(50)
    with page.expect_response(lambda r: '/v3/dashboard/events?' in r.url and 'reasoning_effort=high' in r.url) as response:
        page.locator('#eventEffortFilter').select_option('high')
    rows = response.value.json()['items']
    assert rows and all(row['serviceTier']=='fast' and row['reasoningEffort']=='high' for row in rows)
    expect(page.locator('#eventRows .event-metadata').first).to_contain_text('fast')
    assert 'not confirmation' in page.locator('#eventRows .event-metadata span').first.get_attribute('title')
    page.locator('#loadMore').click()
    expect(page.locator('#eventRows tr')).to_have_count(61)
    expect(page.locator('#loadMore')).to_be_hidden()
    assert [page.locator('#kpiRequests').inner_text(),page.locator('#kpiCost').inner_text()] == totals
    assert all('/v3/dashboard/events?' in url for url in calls[start:] if '/v3/dashboard/' in url), calls[start:]
    # Delay a successful old response beyond the replacement to exercise version/abort guards.
    page.locator('#eventEffortFilter').select_option('')
    page.evaluate('''() => {
      const original = window.fetch;
      window.fetch = async (...args) => {
        const response = await original(...args);
        if (String(args[0]).includes('/events?') && String(args[0]).includes('service_tier=fast')) await new Promise(r => setTimeout(r, 300));
        return response;
      };
      const filter = document.querySelector('#eventTierFilter');
      filter.value = 'fast'; filter.dispatchEvent(new Event('change'));
      setTimeout(() => { filter.value = 'unknown'; filter.dispatchEvent(new Event('change')); }, 30);
    }''')
    expect(page.locator('#eventRows .event-metadata').first).to_contain_text('Unknown')
    page.wait_for_timeout(400)
    expect(page.locator('#eventRows .event-metadata').first).to_contain_text('Unknown')
    expect(page.locator('#errorBanner')).to_be_hidden()
    for lang in ['en-US','zh-CN']:
        if page.locator('html').get_attribute('lang') != lang: page.locator('#languageToggle').click()
        for theme in ['light','dark']:
            if page.locator('html').get_attribute('data-theme') != theme: page.locator('#themeToggle').click()
            for width in [1440,390,320]:
                page.set_viewport_size({'width':width,'height':1000})
                mark = page.locator('#brandMark').bounding_box()
                assert abs(mark['width']-(72 if width>600 else 56))<1 and abs(mark['height']-mark['width'])<1, mark
                assert page.locator('#brandMark').get_attribute('style') is None
                assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
                assert page.locator('.hero-copy h1').evaluate('e=>parseFloat(getComputedStyle(e).fontSize)')==(32 if width>600 else 26)
                assert page.locator('.quota-card').first.evaluate('e=>getComputedStyle(e).backgroundImage')=='none'
                assert page.locator('.quota-card').first.evaluate('e=>getComputedStyle(e,"::before").content')=='none'
                page.locator('.topbar').screenshot(path=str(artifacts/f'fast-header-{lang}-{theme}-{width}.png'))
                if width==390: page.locator('.quota-card').first.screenshot(path=str(artifacts/f'fast-quota-{lang}-{theme}.png'))
    icon = page.request.get(origin+'/dashboard/favicon.svg?v=2')
    assert icon.ok and 'fill="#0b1020"' in icon.text() and 'opacity' not in icon.text()
    preview = page.context.browser.new_page(viewport={'width':400,'height':120})
    preview.set_content(f'<body style="margin:20px;display:flex;gap:20px;background:#fff"><img width="16" height="16" src="{origin}/dashboard/favicon.svg?v=2"><img width="32" height="32" src="{origin}/dashboard/favicon.svg?v=2"><div style="background:#0b1020;padding:10px"><img width="16" height="16" src="{origin}/dashboard/favicon.svg?v=2"></div></body>')
    preview.screenshot(path=str(artifacts/'fast-favicon.png'))
    preview.close()
    assert not errors, errors
    print('PASS: request-only Fast/effort filters, metadata sources, pagination/races, quota diagnostic, neutral cards, fixed brand sizes, locales/themes/mobile and favicon')

fixture.seed = seed
fixture.check = check
if __name__ == '__main__': fixture.main()
