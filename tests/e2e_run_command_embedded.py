#!/usr/bin/env python3
"""Regression test: run_command must not report a false failure when the
engine is embedded in a host process that reaps children process-wide.

kitty installs a SIGCHLD handler and reaps its children; the engine's own
child can be collected by the host before tokio's wait() runs, which returns
ECHILD. The tool used to surface that as `✗ run_command / 没有子进程`, and the
model then retried the same command repeatedly.

Run under the kitty python so the host's real child-reaping is in effect:
    <kitty> +launch tests/e2e_run_command_embedded.py
"""

import ctypes
import json
import os
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOOL_CALL_ID = 'call_1'


class MockLLM(BaseHTTPRequestHandler):
    """Turn 1: ask for a tool call. Turn 2: summarise its result."""
    turn = 0
    tool_result = None
    lock = threading.Lock()

    def do_POST(self):  # noqa: N802
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
        with MockLLM.lock:
            MockLLM.turn += 1
            turn = MockLLM.turn
            for m in body['messages']:
                if m.get('role') == 'tool':
                    MockLLM.tool_result = m.get('content')
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Connection', 'close')
        self.end_headers()

        def sse(obj):
            self.wfile.write(f'data: {json.dumps(obj)}\n\n'.encode())
            self.wfile.flush()
        try:
            if turn == 1:
                sse({'choices': [{'delta': {'tool_calls': [{
                    'index': 0, 'id': TOOL_CALL_ID, 'type': 'function',
                    'function': {'name': 'run_command',
                                 'arguments': json.dumps({'command': 'echo MOSS_OK && exit 0'})},
                }]}}]})
                sse({'choices': [{'delta': {}, 'finish_reason': 'tool_calls'}]})
            else:
                sse({'choices': [{'delta': {'content': 'command finished'}}]})
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

home = tempfile.mkdtemp(prefix='moss-runcmd-')
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
            'models': ['mock-model'], 'default_model': 'mock-model',
            'model_context_window': {'mock-model': 200000},
        }],
        'tools': {'enabled': True},
        'skills': {'allow_command_execution': True},
    }, f)

failures = []


def check(cond, msg):
    print(('  ok   ' if cond else '  FAIL ') + msg)
    if not cond:
        failures.append(msg)


lib = ctypes.CDLL(os.environ.get('MOSS_ENGINE_LIB') or 'libmoss.so', mode=ctypes.RTLD_LOCAL)
lib.moss_init.restype = ctypes.c_int32
lib.moss_ask.restype = ctypes.c_int32
lib.moss_ask.argtypes = [ctypes.c_uint64, ctypes.c_char_p, ctypes.c_size_t]
lib.moss_poll_output.restype = ctypes.c_size_t
lib.moss_poll_output.argtypes = [ctypes.c_uint64, ctypes.c_void_p, ctypes.c_size_t]
lib.moss_stream_state.restype = ctypes.c_int32
lib.moss_stream_state.argtypes = [ctypes.c_uint64]

check(lib.moss_init() == 0, 'engine initialised')

# Reproduce the host-side reaping that caused the false failure: kitty's
# child monitor calls waitpid(-1, ...) periodically. Emulate it aggressively.
stop = threading.Event()


def reaper():
    while not stop.is_set():
        try:
            os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            pass
        except Exception:
            pass
        time.sleep(0.001)


threading.Thread(target=reaper, daemon=True).start()

q = '运行 echo'.encode()
check(lib.moss_ask(1, q, len(q)) == 0, 'ask accepted')

buf = ctypes.create_string_buffer(65536)
out = bytearray()
deadline = time.monotonic() + 40
while time.monotonic() < deadline:
    n = lib.moss_poll_output(1, buf, len(buf))
    if n:
        out += buf.raw[:n]
        continue
    if lib.moss_stream_state(1) == 0:
        break
    time.sleep(0.02)
stop.set()

text = out.decode('utf-8', 'replace')
print('--- injected terminal output ---')
print(text.replace('\x1b', '\\e')[:600])
print('--- tool result seen by the model ---')
print((MockLLM.tool_result or '(none)')[:400])

check(MockLLM.tool_result is not None, 'the tool result reached the model')
if MockLLM.tool_result:
    check('MOSS_OK' in MockLLM.tool_result, 'command stdout captured')
    try:
        parsed = json.loads(MockLLM.tool_result)
        check(parsed.get('success') is True,
              f'reported success (got {parsed.get("success")!r})')
    except json.JSONDecodeError:
        check(False, 'tool result is valid JSON')
check('没有子进程' not in text and 'No child process' not in text,
      'no ECHILD false failure surfaced')
check(MockLLM.turn == 2, f'exactly one tool round-trip (turns={MockLLM.turn})')

lib.moss_shutdown()
server.shutdown()
print()
if failures:
    print(f'FAILED: {len(failures)} check(s)')
    for f in failures:
        print('  - ' + f)
    sys.exit(1)
print('ALL CHECKS PASSED')
