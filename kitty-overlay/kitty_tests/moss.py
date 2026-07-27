#!/usr/bin/env python
# License: GPLv3 Copyright: 2026, Moss Terminal contributors

from . import BaseTest, parse_bytes


class MossIntegration(BaseTest):
    def test_osc_7717_dispatches_to_callbacks(self):
        s = self.create_screen()
        received = []
        s.callbacks.moss_osc = lambda payload: received.append(payload)
        parse_bytes(s, b'\x1b]7717;ask;aGVsbG8=\x1b\\')
        self.ae(received, ['ask;aGVsbG8='])
        parse_bytes(s, b'\x1b]7717;cancel\x1b\\')
        self.ae(received, ['ask;aGVsbG8=', 'cancel'])

    def test_osc_7717_without_handler_is_harmless(self):
        # A Screen whose callbacks lack moss_osc must not break parsing.
        s = self.create_screen()
        parse_bytes(s, b'\x1b]7717;ask;xxx\x1b\\')
        parse_bytes(s, b'hello')
        self.ae(str(s.line(0)), 'hello')

    def test_line_capture_hooks_do_not_disturb_screen_state(self):
        # The moss line hook runs on every linefeed (engine library absent in
        # tests -> no-op path). Verify normal flow including prompt marking.
        s = self.create_screen()
        parse_bytes(s, b'\x1b]133;A\x07$ ls\r\n\x1b]133;C\x07file1\r\nfile2\r\n')
        self.ae(str(s.line(0)), '$ ls')
        self.ae(str(s.line(1)), 'file1')
        self.ae(str(s.line(2)), 'file2')

    def test_cwd_notification_still_recorded(self):
        s = self.create_screen()
        parse_bytes(s, b'\x1b]7;file://host/tmp/somewhere\x1b\\')
        self.ae(s.last_reported_cwd, b'file://host/tmp/somewhere')
