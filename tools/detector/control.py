#!/usr/bin/env python3
"""Control run for tools/detector: drive Chrome the *noisy* way and show the
detector flagging it. Needs `pip install websocket-client` and a running
surf-testserver (`cargo run -p surf-testserver -- 48123`).

    python3 tools/detector/control.py [chrome-path] [detector-url]

Launches Chrome with --remote-debugging-port and --enable-automation (what
most frameworks do), sends Runtime.enable on the page session (what
Puppeteer / Playwright / Selenium's CDP do), loads the detector and prints
every <li data-check> result. Expected: runtime-enable-stack FAIL,
webdriver FAIL. Compare with `surf run tests/e2e/scripts/detector.surf`.
"""
import json
import os
import subprocess
import sys
import tempfile
import time

import websocket  # websocket-client

CHROME = sys.argv[1] if len(sys.argv) > 1 else os.environ.get(
    "SURF_CHROME", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
URL = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:48123/detector?mode=headless"

profile = tempfile.mkdtemp(prefix="detector-control-")
proc = subprocess.Popen(
    [CHROME, "--headless", "--remote-debugging-port=0", f"--user-data-dir={profile}",
     "--no-first-run", "--no-default-browser-check", "--enable-automation", "about:blank"],
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
port_file = os.path.join(profile, "DevToolsActivePort")
for _ in range(100):
    if os.path.exists(port_file):
        break
    time.sleep(0.1)
port, path = open(port_file).read().split("\n")[:2]
ws = websocket.create_connection(f"ws://127.0.0.1:{port}{path}", suppress_origin=True)
seq = 0


def call(method, params=None, session=None):
    global seq
    seq += 1
    msg = {"id": seq, "method": method, "params": params or {}}
    if session:
        msg["sessionId"] = session
    ws.send(json.dumps(msg))
    while True:
        m = json.loads(ws.recv())
        if m.get("id") == seq:
            return m.get("result", m)


target = call("Target.createTarget", {"url": "about:blank"})["targetId"]
session = call("Target.attachToTarget", {"targetId": target, "flatten": True})["sessionId"]
call("Runtime.enable", session=session)  # the side effect under test
call("Page.navigate", {"url": URL}, session=session)
time.sleep(1.5)
res = call("Runtime.evaluate", {
    "expression": "[...document.querySelectorAll('li[data-check]')].map(l => [l.dataset.check, l.dataset.status, l.dataset.detail])",
    "returnByValue": True}, session=session)
for check, status, detail in res["result"]["value"]:
    print(f"{status:5} {check:24} {detail}")
ws.close()
proc.terminate()
proc.wait()
