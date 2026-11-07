#!/usr/bin/env python3
"""Real browser fork/clone check with a temporary offline native host."""
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
            page.locator('#prompt').fill('browser fork source');page.locator('#submit').click();expect(page.locator('#output')).to_contain_text('Reviewed input: browser fork source');expect(page.locator('#clone')).to_be_enabled()
            auth={'Authorization':'Bearer '+TOKEN}
            state=page.request.get(url+'/api/state',headers=auth).json();source=state['session_id']
            page.locator('#clone').click()
            expect(page.locator('#output')).to_contain_text('New forked conversation ready')
            expect(page.locator('#fork')).to_be_enabled()
            state=page.request.get(url+'/api/state',headers=auth).json();clone=state['session_id'];assert clone!=source
            page.locator('#fork').click()
            expect(page.locator('#prompt')).to_have_value('browser fork source')
            state=page.request.get(url+'/api/state',headers=auth).json();fork=state['session_id'];assert fork not in (source,clone)
            assert state['run']['request_id']==1, 'restoring a fork draft submitted a model request'
            if args.screenshot: page.screenshot(path=str(args.screenshot),full_page=True)
            page.locator('#submit').click();expect(page.locator('#submit')).to_be_enabled()
            state=page.request.get(url+'/api/state',headers=auth).json();assert state['run']['session_id']==fork
            assert not errors,errors
            print(json.dumps({'browser':'chromium','version':browser.version,'authenticated':True,'clone_new_identity':True,'fork_before_prompt':True,'draft_only':True,'explicit_continuation':True,'external_model_requests':0}))
        finally: browser.close()
finally:
    child.terminate()
    try: child.wait(timeout=5)
    except subprocess.TimeoutExpired: child.kill();child.wait()
