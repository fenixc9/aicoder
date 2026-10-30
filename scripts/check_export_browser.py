#!/usr/bin/env python3
"""Optional static-export browser check; uses the same dev-only Playwright as the Web check."""
import argparse
import json
from pathlib import Path
from playwright.sync_api import sync_playwright
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("html", type=Path)
parser.add_argument("json", type=Path)
parser.add_argument("--screenshot", type=Path)
args = parser.parse_args()
document = args.html.read_text()
canonical = json.loads(args.json.read_text())
with sync_playwright() as p:
    browser = p.chromium.launch(headless=True, args=["--no-sandbox"])
    try:
        page = browser.new_page(viewport={"width": 1100, "height": 1000})
        requests = []
        def route(request):
            requests.append(request.request.url)
            if request.request.url == "http://export.invalid/":
                request.fulfill(status=200, content_type="text/html", body=document)
            else:
                request.abort()
        page.route("**/*", route)
        page.goto("http://export.invalid/")
        assert page.locator("article").count() == canonical["manifest"]["entry_count"]
        assert page.locator("article.selected").count() == sum(e["selected_branch"] for e in canonical["entries"])
        assert page.locator("script, img, iframe, object, form").count() == 0
        assert "<script>window.injected=true</script>" in page.locator("body").inner_text()
        assert "https://remote.invalid/x" in page.locator("body").inner_text()
        assert requests == ["http://export.invalid/"]
        if args.screenshot:
            page.screenshot(path=str(args.screenshot), full_page=True)
        print(json.dumps({"browser": browser.version, "canonical_entries": len(canonical["entries"]), "inert_model_html": True, "external_requests": 0}))
    finally:
        browser.close()
