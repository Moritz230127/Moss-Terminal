#!/usr/bin/env python3
"""Regression: the streamed AI answer must NOT leak into the next increment.

Reproduces the acceptance-audit finding: kitty injects answer bytes on a
30ms timer AFTER the engine's turn future finished (busy already false); the
injection re-enters via moss_on_line and used to pollute the next ask's
【终端上下文】 with the previous answer's tail.

Simulates the exact kitty ordering with plain ctypes (no kitty needed):
every drained chunk is fed back line-by-line through moss_on_line, then the
injector calls moss_set_capture(wid, 1) exactly where moss_integration.py
does — after the final drain, before the sentinel.
"""

import ctypes
import json
import os
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ANSWER = 'ANSWER_0: the fix is to add a semicolon'


class MockLLM(BaseHTTPRequestHandler):
    requests = []
    lock = threading.Lock()

    def do_POST(self):  # noqa: N802
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
        with MockLLM.lock:
            MockLLM.requests.append(body)
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Connection', 'close')
        self.end_headers()

        def sse(obj):
            self.wfile.write(f'data: {json.dumps(obj)}\n\n'.encode())
            self.wfile.flush()
        try:
            sse({'choices': [{'delta': {'content': ANSWER}}]})
            sse({'choices': [{'delta': {}, 'finish_reason': 'stop'}]})
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *a):
        pass


server = ThreadingHTTPServer(('127.0.0.1', 0), MockLLM)
port = server.server_address[1]
threading.Thread(target=server.serve_forever, daemon=True).start()

home = tempfile.mkdtemp(prefix='moss-leak-')
os.environ['MOSS_HOME'] = home
os.environ['MOSS_LOG'] = 'off'
cfg = os.path.join(home, 'config')
os.makedirs(cfg, exist_ok=True)
with open(os.path.join(cfg, 'config.jsonc'), 'w') as f:
    json.dump({
        'active_provider': 'mock',
        'providers': [{
            'id': 'mock', 'display_name': 'Mock',
            'base_url': f'http://127.0.0.1:{port}/v1',
            'protocol': 'openai-chat', 'api_key': 'k',
            'models': ['m'], 'default_model': 'm',
            'model_context_window': {'m': 200000},
        }],
        'tools': {'enabled': False},
    }, f)

repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
lib = ctypes.CDLL(
    os.environ.get('MOSS_ENGINE_LIB') or os.path.join(repo, 'engine/target/release/libmoss.so'),
    mode=ctypes.RTLD_LOCAL)
lib.moss_init.restype = ctypes.c_int32
lib.moss_ask.restype = ctypes.c_int32
lib.moss_ask.argtypes = [ctypes.c_uint64, ctypes.c_char_p, ctypes.c_size_t]
lib.moss_on_line.argtypes = [ctypes.c_uint64, ctypes.c_uint8, ctypes.c_char_p, ctypes.c_size_t]
lib.moss_poll_output.restype = ctypes.c_size_t
lib.moss_poll_output.argtypes = [ctypes.c_uint64, ctypes.c_void_p, ctypes.c_size_t]
lib.moss_stream_state.restype = ctypes.c_int32
lib.moss_stream_state.argtypes = [ctypes.c_uint64]
lib.moss_set_capture.argtypes = [ctypes.c_uint64, ctypes.c_int32]

assert lib.moss_init() == 0
WID = 7


def feed(text, kind=3):
    b = text.encode()
    lib.moss_on_line(WID, kind, b, len(b))


def ask_and_drain(question):
    q = question.encode()
    rc = lib.moss_ask(WID, q, len(q))
    assert rc == 0, rc
    buf = ctypes.create_string_buffer(65536)
    pending = ''
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        n = lib.moss_poll_output(WID, buf, len(buf))
        if n:
            # Faithful kitty behaviour: injected bytes re-enter the hook,
            # one completed line per linefeed.
            pending += buf.raw[:n].decode('utf-8', 'replace')
            while '\n' in pending:
                line, pending = pending.split('\n', 1)
                feed(line.strip('\r'))
            continue
        if lib.moss_stream_state(WID) == 0:
            break
        time.sleep(0.03)
    # kitty's injector: unmute only after everything is on screen.
    lib.moss_set_capture(WID, 1)


feed('user@host ~> gcc main.c', 1)
feed('error: expected semicolon')
ask_and_drain('why does it fail')

# New user activity after the answer.
feed('user@host ~> make', 1)
feed('build ok')
ask_and_drain('now what')

user2 = ''
for m in reversed(MockLLM.requests[-1]['messages']):
    if m['role'] == 'user':
        user2 = m['content']
        break
print('--- second ask user message ---')
print(user2)
print('---')

failures = []
if 'ANSWER_0' in user2.split('【问题】')[0]:
    failures.append('previous answer leaked into next increment')
if 'make' not in user2 or 'build ok' not in user2:
    failures.append('genuine new lines missing from increment')

lib.moss_shutdown()
server.shutdown()
if failures:
    print('FAILED:', '; '.join(failures))
    sys.exit(1)
print('ALL CHECKS PASSED')
