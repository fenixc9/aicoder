#!/usr/bin/env python3
"""Optional real browser check of the offline native UI host; fresh temporary browser context."""
import argparse
import json
import os
from pathlib import Path
import selectors
import subprocess
from playwright.sync_api import sync_playwright, expect
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/examples/offline_web_extension_ui')
parser.add_argument('--screenshot',type=Path)
args=parser.parse_args()
TOKEN='fixture-ui-browser-0123456789abcdef'
child=subprocess.Popen([str(args.binary.resolve())],env={'PATH':os.environ.get('PATH',''),'AICODER_WEB_TOKEN':TOKEN},stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
try:
    selector=selectors.DefaultSelector();selector.register(child.stdout,selectors.EVENT_READ)
    if not selector.select(10): raise RuntimeError('offline Web host did not start')
    config=json.loads(child.stdout.readline());assert config['offline'];url=config['url']
    with sync_playwright() as p:
        browser=p.chromium.launch(headless=True,args=['--no-sandbox'])
        try:
            page=browser.new_page(viewport={'width':1100,'height':1100})
            errors=[];page.on('pageerror',lambda error:errors.append(str(error)))
            page.goto(url);page.locator('#token').fill(TOKEN);page.locator('#connect').click();expect(page.locator('#extension-commands')).to_be_visible()
            page.locator('#extension-command').select_option('review');page.locator('#extension-run').click();expect(page.locator('#extension-title')).to_have_text('Review action')
            page.locator('#extension-options').select_option('notice');page.locator('#extension-answer').click();expect(page.locator('#extension-title')).to_have_text('Review note')
            injection='<img src=x onerror="window.__ui_xss=true">';page.evaluate('window.__ui_xss=false');page.locator('#extension-input').fill(injection)
            state=page.request.get(url+'/api/state',headers={'Authorization':'Bearer '+TOKEN}).json();assert state['run'] is None;assert state['command']['result'] is None
            if args.screenshot: page.screenshot(path=str(args.screenshot),full_page=True)
            page.locator('#extension-answer').click();expect(page.locator('#output')).to_contain_text('Review noted: '+injection);assert page.evaluate('window.__ui_xss') is False
            expect(page.locator('#extension-run')).to_be_enabled();page.locator('#extension-run').click();expect(page.locator('#extension-title')).to_have_text('Review action')
            page.locator('#extension-options').select_option('submit');page.locator('#extension-answer').click();expect(page.locator('#extension-title')).to_have_text('Review note')
            page.locator('#extension-input').fill('to agent');page.locator('#extension-answer').click();expect(page.locator('#output')).to_contain_text('Reviewed input: Review request: to agent')
            expect(page.locator('#extension-run')).to_be_enabled();page.locator('#extension-run').click();expect(page.locator('#extension-title')).to_have_text('Review action');page.locator('#cancel').click();expect(page.locator('#extension-question')).to_be_hidden()
            assert not errors,errors
            print(json.dumps({'browser':'chromium','version':browser.version,'authenticated':True,'commands':True,'selection_input_status_notice':True,'inert_text':True,'core_submit':True,'cancel':True}))
        finally: browser.close()
finally:
    child.terminate()
    try: child.wait(timeout=5)
    except subprocess.TimeoutExpired: child.kill();child.wait()
