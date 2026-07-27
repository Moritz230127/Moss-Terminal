#!/usr/bin/env python3
"""End-to-end capture test through the REAL kitty C hook.

Unlike tests/e2e_mock_llm.py (which calls moss_on_line directly), this drives
a real kitty Screen with real terminal bytes, so screen_linefeed ->
moss_on_line_added -> extract_line_text -> moss_on_line is exercised exactly
as it is in a live window. It then asks a question and asserts the captured
terminal output actually reached the LLM request.

Must run under the kitty python:
    MOSS_ENGINE_LIB=<libmoss.so> <kitty> +runpy \
        "exec(open('tests/e2e_kitty_capture.py').read())"
or via tests/run_kitty_capture.sh
"""

import base64
import ctypes
import json
import os
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__))) if '__file__' in dir() else os.getcwd()

# --- mock LLM ---------------------------------------------------------------

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
            sse({'choices': [{'delta': {'reasoning_content': 'checking the listing'}}]})
            # Deliberately Markdown-heavy, to prove the degrader works on the
            # real injection path too.
            for piece in ('## Result\n', 'The **largest** entry is `WarThunder`.\n',
                          '```bash\ndu -sh ~/WarThunder\n```\n',
                          '| name | size |\n|---|---|\n| WarThunder | 146G |\n', 'done'):
                sse({'choices': [{'delta': {'content': piece}}]})
            sse({'choices': [{'delta': {}, 'finish_reason': 'stop'}],
                 'usage': {'prompt_tokens': 10, 'completion_tokens': 5, 'total_tokens': 15}})
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *a):
        pass


# --- sandbox setup ----------------------------------------------------------

server = ThreadingHTTPServer(('127.0.0.1', 0), MockLLM)
port = server.server_address[1]
threading.Thread(target=server.serve_forever, daemon=True).start()

home = tempfile.mkdtemp(prefix='moss-capture-')
os.environ['MOSS_HOME'] = home
os.environ['MOSS_LOG'] = 'off'
cfg_dir = os.path.join(home, 'config')
os.makedirs(cfg_dir, exist_ok=True)
with open(os.path.join(cfg_dir, 'config.jsonc'), 'w') as f:
    json.dump({
        'active_provider': 'mock',
        'providers': [{
            'id': 'mock', 'display_name': 'Mock',
            'base_url': f'http://127.0.0.1:{port}/v1',
            'protocol': 'openai-chat', 'api_key': 'k',
            'models': ['mock-model'], 'default_model': 'mock-model',
            'model_context_window': {'mock-model': 200000},
        }],
        'tools': {'enabled': False},
    }, f)

# --- real kitty screen ------------------------------------------------------

from kitty.fast_data_types import Screen  # noqa: E402
from kitty_tests import Callbacks, parse_bytes  # noqa: E402

failures = []

def check(cond, msg):
    print(('  ok   ' if cond else '  FAIL ') + msg)
    if not cond:
        failures.append(msg)

lib_path = os.environ.get('MOSS_ENGINE_LIB') or 'libmoss.so'
lib = ctypes.CDLL(lib_path, mode=ctypes.RTLD_LOCAL)
lib.moss_init.restype = ctypes.c_int32
lib.moss_ask.restype = ctypes.c_int32
lib.moss_ask.argtypes = [ctypes.c_uint64, ctypes.c_char_p, ctypes.c_size_t]
lib.moss_poll_output.restype = ctypes.c_size_t
lib.moss_poll_output.argtypes = [ctypes.c_uint64, ctypes.c_void_p, ctypes.c_size_t]
lib.moss_stream_state.restype = ctypes.c_int32
lib.moss_stream_state.argtypes = [ctypes.c_uint64]
rc = lib.moss_init()
check(rc == 0, f'moss_init returned {rc}')

WINDOW_ID = 0  # kitty_tests screens use 0; the hook must tolerate it

cb = Callbacks()
s = Screen(cb, 40, 120, 5000, 10, 20, WINDOW_ID, cb)

# Bytes exactly like a real session: OSC 133 prompt marks, SGR colours,
# Nerd Font glyphs (private use area), CJK filenames.
ESC = b'\x1b'
prompt = (ESC + b']133;A\x07'
          + b'\xf3\xb0\xa3\x87 \x1b[36mArch\x1b[0m  \x1b[33m~\x1b[0m   19:24\r\n')
cmd = ESC + b']133;C;cmdline=ls -lh\x07' + b'ls -lh\r\n'
listing = (
    b'\x1b[1mPermissions Size User Date Modified Name\x1b[0m\r\n'
    b'drwxr-xr-x     - Arch 26 7\xe6\x9c\x88  17:41 \xef\x84\x95 Desktop\r\n'
    b'.rwxr-xr-x   448 Arch 22 7\xe6\x9c\x88  15:06 \xef\x92\x89 marker_env.sh\r\n'
    b'.rw-r--r--   15k Arch 25 7\xe6\x9c\x88  18:00 \xef\x92\x8d tkg-from-scratch-prompt.md\r\n'
    b'drwxrwxr-x     - Arch 25 7\xe6\x9c\x88  19:49 \xef\x84\x95 WarThunder\r\n'
    b'.rw-r--r--  8.0k Arch 22 7\xe6\x9c\x88  00:04 \xef\x92\x89 \xe5\xbf\x85\xe7\x9c\x8b-Shorin-DMS-Niri\xe4\xbd\xbf\xe7\x94\xa8\xe6\x96\xb9\xe6\xb3\x95.txt\r\n'
)
after = ESC + b']133;D;0\x07'
trigger_prompt = (ESC + b']133;A\x07'
                  + b'\xf3\xb0\xa3\x87 \x1b[36mArch\x1b[0m  \x1b[33m~\x1b[0m   19:24\r\n'
                  + b' \xe3\x80\x8b \xe4\xb8\x8a\xe9\x9d\xa2\xe8\xbe\x93\xe5\x87\xba\xe9\x87\x8c\xe6\x9c\x80\xe5\xa4\xa7\xe7\x9a\x84\xe6\x96\x87\xe4\xbb\xb6\xe6\x98\xaf\xe5\x93\xaa\xe4\xb8\xaa\r\n')

print('feeding terminal bytes through the real C hook...')
parse_bytes(s, prompt + cmd + listing + after + trigger_prompt)

# Sanity: the screen itself rendered the listing.
screen_text = '\n'.join(str(s.line(i)) for i in range(12))
check('WarThunder' in screen_text, 'screen rendered WarThunder')
check('必看-Shorin-DMS-Niri使用方法.txt' in screen_text, 'screen rendered CJK filename')

question = '上面输出里面最大的文件是哪个'.encode()
rc = lib.moss_ask(WINDOW_ID, question, len(question))
check(rc == 0, f'moss_ask returned {rc}')

buf = ctypes.create_string_buffer(65536)
out = bytearray()
deadline = time.monotonic() + 30
while time.monotonic() < deadline:
    n = lib.moss_poll_output(WINDOW_ID, buf, len(buf))
    if n:
        out += buf.raw[:n]
        continue
    if lib.moss_stream_state(WINDOW_ID) == 0:
        break
    time.sleep(0.02)

check(bool(MockLLM.requests), 'the LLM was called')
if MockLLM.requests:
    user_msg = ''
    for m in reversed(MockLLM.requests[0]['messages']):
        if m['role'] == 'user':
            user_msg = m['content']
            break
    print('--- user message sent to the LLM ---')
    print(user_msg[:1200])
    print('--- end ---')
    check('【终端上下文】' in user_msg, 'context block present')
    check('ls -lh' in user_msg, 'the command line was captured')
    check('WarThunder' in user_msg, 'listing rows were captured')
    check('tkg-from-scratch-prompt.md' in user_msg, 'file names captured')
    check('必看-Shorin-DMS-Niri' in user_msg, 'CJK filename captured (grapheme extraction)')
    check('?' not in user_msg.replace('？', ''), 'no ? placeholders from failed glyph extraction')
    check(user_msg.count('》') <= 1, 'trigger line not duplicated into the context')

text = out.decode('utf-8', 'replace')
check('```' not in text, 'no markdown fences in injected output')
check('**' not in text, 'no bold markers in injected output')
check('## ' not in text, 'no markdown headings in injected output')
check('|---' not in text, 'no table separator rows in injected output')
check('WarThunder' in text, 'answer text reached the terminal')
check('\x1b_moss;rs' in text, 'reasoning start marker emitted')
check('\x1b_moss;re;' in text, 'reasoning end marker emitted')

lib.moss_shutdown()
server.shutdown()
print()
if failures:
    print(f'FAILED: {len(failures)} check(s)')
    for f in failures:
        print('  - ' + f)
    sys.exit(1)
print('ALL CHECKS PASSED')
