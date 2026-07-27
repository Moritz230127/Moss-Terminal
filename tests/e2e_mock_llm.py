#!/usr/bin/env python3
"""End-to-end test of the embedded Moss engine against a mock LLM.

Loads libmoss.so via ctypes exactly like kitty's moss_integration.py does,
points it at a local mock OpenAI-compatible SSE server, and drives the full
loop: line capture -> 》ask -> streamed styled output -> multi-turn
increments -> per-window isolation -> cancellation.

Fully sandboxed: MOSS_HOME redirects every engine path into a temp dir; the
only network traffic is to 127.0.0.1. No host state is touched.

Run:  python3 tests/e2e_mock_llm.py
Env:  MOSS_E2E_LIB=/path/to/libmoss.so (default: engine/target/release/libmoss.so)
"""

import ctypes
import json
import os
import re
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LIB_PATH = os.environ.get('MOSS_E2E_LIB', os.path.join(REPO, 'engine/target/release/libmoss.so'))

ANSI_RE = re.compile(r'\x1b\[[0-9;]*m')


class MockLLM(BaseHTTPRequestHandler):
    requests = []          # list of parsed request bodies, in arrival order
    lock = threading.Lock()

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get('Content-Length', 0))
        body = json.loads(self.rfile.read(length))
        with MockLLM.lock:
            index = len(MockLLM.requests)
            MockLLM.requests.append(body)

        last_user = ''
        for msg in reversed(body.get('messages', [])):
            if msg.get('role') == 'user':
                content = msg.get('content', '')
                last_user = content if isinstance(content, str) else json.dumps(content)
                break
        slow = 'SLOW' in last_user

        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Connection', 'close')
        self.end_headers()

        def sse(obj):
            self.wfile.write(f'data: {json.dumps(obj)}\n\n'.encode())
            self.wfile.flush()

        try:
            if slow:
                # Long trickle so cancellation lands mid-stream.
                for tick in range(200):
                    sse({'choices': [{'delta': {'content': f'slow{tick} '}}]})
                    time.sleep(0.05)
            else:
                sse({'choices': [{'delta': {'reasoning_content': 'pondering...'}}]})
                for piece in (f'ANSWER_{index}:', ' all', ' good'):
                    sse({'choices': [{'delta': {'content': piece}}]})
            sse({'choices': [{'delta': {}, 'finish_reason': 'stop'}],
                 'usage': {'prompt_tokens': 10, 'completion_tokens': 5, 'total_tokens': 15}})
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass  # client cancelled — expected in the cancellation test

    def log_message(self, *a):  # silence
        pass


class E2E(unittest.TestCase):
    lib = None
    home = None
    server = None
    port = 0

    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(('127.0.0.1', 0), MockLLM)
        cls.port = cls.server.server_address[1]
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

        cls.home = tempfile.mkdtemp(prefix='moss-e2e-')
        os.environ['MOSS_HOME'] = cls.home
        os.environ['MOSS_LOG'] = 'off'
        config_dir = os.path.join(cls.home, 'config')
        os.makedirs(config_dir, exist_ok=True)
        config = {
            'active_provider': 'mock',
            'providers': [{
                'id': 'mock',
                'display_name': 'Mock LLM',
                'base_url': f'http://127.0.0.1:{cls.port}/v1',
                'protocol': 'openai-chat',
                'api_key': '$env:MOSS_MOCK_API_KEY',
                'models': ['mock-model'],
                'default_model': 'mock-model',
            }],
            'tools': {'enabled': False},
        }
        with open(os.path.join(config_dir, 'config.jsonc'), 'w') as f:
            json.dump(config, f, indent=2)
        # Key resolution must go through the .env fallback (regression test
        # for the missing-.env-loader defect).
        with open(os.path.join(config_dir, '.env'), 'w') as f:
            f.write('MOSS_MOCK_API_KEY="mock-secret"\n')
        os.environ.pop('MOSS_MOCK_API_KEY', None)

        lib = ctypes.CDLL(LIB_PATH, mode=ctypes.RTLD_LOCAL)
        u64, size_t = ctypes.c_uint64, ctypes.c_size_t
        lib.moss_init.restype = ctypes.c_int32
        lib.moss_ask.restype = ctypes.c_int32
        lib.moss_ask.argtypes = [u64, ctypes.c_char_p, size_t]
        lib.moss_on_line.argtypes = [u64, ctypes.c_uint8, ctypes.c_char_p, size_t]
        lib.moss_on_prompt_mark.argtypes = [u64, ctypes.c_uint8, ctypes.c_char_p, size_t]
        lib.moss_on_cwd.argtypes = [u64, ctypes.c_char_p, size_t]
        lib.moss_poll_output.restype = size_t
        lib.moss_poll_output.argtypes = [u64, ctypes.c_void_p, size_t]
        lib.moss_stream_state.restype = ctypes.c_int32
        lib.moss_stream_state.argtypes = [u64]
        lib.moss_set_capture.restype = None
        lib.moss_set_capture.argtypes = [u64, ctypes.c_int32]
        lib.moss_cancel.argtypes = [u64]
        lib.moss_window_closed.argtypes = [u64]
        cls.lib = lib
        rc = lib.moss_init()
        if rc != 0:
            raise RuntimeError(f'moss_init failed: {rc}')

    @classmethod
    def tearDownClass(cls):
        cls.lib.moss_shutdown()
        cls.server.shutdown()

    # -- helpers ----------------------------------------------------------

    def feed_line(self, win, text, kind=3):
        data = text.encode()
        self.lib.moss_on_line(win, kind, data, len(data))

    def ask(self, win, question):
        q = question.encode()
        return self.lib.moss_ask(win, q, len(q))

    def drain(self, win, timeout=30.0):
        buf = ctypes.create_string_buffer(65536)
        out = bytearray()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            n = self.lib.moss_poll_output(win, buf, len(buf))
            if n:
                out += buf.raw[:n]
                continue
            if self.lib.moss_stream_state(win) == 0:
                # Follow the injector contract (moss_integration.py): line
                # capture is muted from ask() until the injector reports the
                # answer fully written to the screen.
                self.lib.moss_set_capture(win, 1)
                return bytes(out)
            time.sleep(0.02)
        raise AssertionError(f'stream did not finish; got so far: {out[:400]!r}')

    @staticmethod
    def last_user_content(request):
        for msg in reversed(request['messages']):
            if msg['role'] == 'user':
                return msg['content']
        return ''

    # -- tests (ordered by name) -------------------------------------------

    def test_00_basic_ask_with_context(self):
        win = 101
        self.feed_line(win, 'user@host ~> gcc main.c', kind=1)
        self.feed_line(win, "error: expected ';' before 'return'", kind=3)
        self.lib.moss_on_prompt_mark(win, ord('D'), b'1', 1)
        cwd = b'file://host/tmp/moss%20project'
        self.lib.moss_on_cwd(win, cwd, len(cwd))
        self.feed_line(win, 'user@host ~> 》why does it fail', kind=1)

        self.assertEqual(self.ask(win, '》why does it fail'), 0)
        out = self.drain(win)
        text = ANSI_RE.sub('', out.decode())
        self.assertIn('ANSWER_0: all good', text)
        # Reasoning tokens must arrive green (styled) before the answer.
        self.assertIn('\x1b[38;5;10m', out.decode())
        self.assertIn('pondering', out.decode())

        req = MockLLM.requests[0]
        user = self.last_user_content(req)
        self.assertIn('【终端上下文】', user)
        self.assertIn('gcc main.c', user)
        self.assertIn("expected ';'", user)
        self.assertIn('[exit status: 1]', user)
        self.assertIn('【工作目录】/tmp/moss project', user)
        self.assertIn('【问题】', user)
        self.assertIn('why does it fail', user)
        # The 》 trigger line itself must not be duplicated into context.
        self.assertNotIn('》why does it fail\n', user.split('【问题】')[0])

    def test_01_multi_turn_incremental_context(self):
        win = 101
        self.feed_line(win, 'user@host ~> ls', kind=1)
        self.feed_line(win, 'main.c  main.o', kind=3)
        self.assertEqual(self.ask(win, 'what changed'), 0)
        text = ANSI_RE.sub('', self.drain(win).decode())
        self.assertIn('ANSWER_1', text)

        req = MockLLM.requests[1]
        user = self.last_user_content(req)
        self.assertIn('main.c  main.o', user)
        # Incremental: turn-1 context must NOT be resent inside turn-2 input…
        self.assertNotIn('gcc main.c', user)
        # …but the conversation history must carry turn 1 as prior messages.
        all_text = json.dumps(req['messages'], ensure_ascii=False)
        self.assertIn('ANSWER_0', all_text)
        self.assertIn('why does it fail', all_text)

    def test_02_window_isolation(self):
        win = 202
        self.feed_line(win, 'user@host ~> pacman -Syu', kind=1)
        self.assertEqual(self.ask(win, 'is this safe'), 0)
        text = ANSI_RE.sub('', self.drain(win).decode())
        self.assertIn('ANSWER_2', text)

        req = MockLLM.requests[2]
        all_text = json.dumps(req['messages'], ensure_ascii=False)
        self.assertIn('pacman -Syu', all_text)
        # Nothing from window 101 may leak into window 202's conversation.
        self.assertNotIn('gcc main.c', all_text)
        self.assertNotIn('ANSWER_0', all_text)
        # Per-window state dirs exist.
        self.assertTrue(os.path.isdir(os.path.join(self.home, 'state/windows/101')))
        self.assertTrue(os.path.isdir(os.path.join(self.home, 'state/windows/202')))

    def test_03_busy_and_cancel(self):
        win = 303
        self.assertEqual(self.ask(win, 'please be SLOW about it'), 0)
        # Wait until streaming demonstrably started.
        deadline = time.monotonic() + 10
        buf = ctypes.create_string_buffer(65536)
        got = bytearray()
        while time.monotonic() < deadline and b'slow' not in got:
            n = self.lib.moss_poll_output(win, buf, len(buf))
            got += buf.raw[:n]
            time.sleep(0.02)
        self.assertIn(b'slow', got, 'stream never started')
        # A second ask on the same window while busy must be rejected.
        self.assertEqual(self.ask(win, 'another one'), -2)
        # Cancel and confirm the stream terminates with the interrupt notice.
        self.lib.moss_cancel(win)
        rest = self.drain(win, timeout=15)
        text = ANSI_RE.sub('', (bytes(got) + rest).decode())
        self.assertIn('[interrupted]', text)
        self.assertEqual(self.lib.moss_stream_state(win), 0)

    def test_04_window_closed_cleans_session(self):
        self.lib.moss_window_closed(303)
        # A fresh ask after close starts a new empty-context session.
        self.assertEqual(self.ask(303, 'fresh start'), 0)
        text = ANSI_RE.sub('', self.drain(303).decode())
        self.assertIn('ANSWER_', text)
        user = self.last_user_content(MockLLM.requests[-1])
        self.assertNotIn('【终端上下文】', user)


if __name__ == '__main__':
    if not os.path.exists(LIB_PATH):
        print(f'error: {LIB_PATH} not built; run: cd engine && cargo build --release --no-default-features', file=sys.stderr)
        sys.exit(2)
    unittest.main(verbosity=2)
