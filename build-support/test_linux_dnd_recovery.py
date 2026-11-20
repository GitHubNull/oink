#!/usr/bin/env python3
"""Offline tests for the D-wave recovery supervisor primitives (no X server)."""
import os
from pathlib import Path
import selectors
import signal
import socket
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))

import linux_dnd_recovery as recovery
from linux_dnd_protocol import Failure


class FakeRole:
    """Role surface used by expect_eof: nonblocking pipes plus close()."""

    def __init__(self, process):
        self.process = process
        self.name = 'fake-role'
        self.frames = []
        self.expected_exit = None
        self.stream_sock, peer = socket.socketpair()
        peer.close()
        self.selector = selectors.DefaultSelector()
        os.set_blocking(process.stdout.fileno(), False)

    def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=1)
        self.selector.close()
        self.stream_sock.close()
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()


def spawn_sleep():
    process = subprocess.Popen(['sleep', '30'], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
    return FakeRole(process)


class ProcessTokenTests(unittest.TestCase):
    def test_token_returns_starttime_for_owned_child(self):
        role = spawn_sleep()
        try:
            token = recovery.process_token(role.process)
            self.assertIsInstance(token, str)
            stat = Path(f'/proc/{role.process.pid}/stat').read_text()
            self.assertEqual(token, stat.rsplit(')', 1)[1].split()[19])
        finally:
            role.close()

    def test_dead_child_yields_no_token(self):
        role = spawn_sleep()
        role.process.kill()
        role.process.wait(timeout=2)
        self.assertIsNone(recovery.process_token(role.process))
        role.close()

    def test_joint_identity_registers_two_live_children(self):
        # Kernel starttime granularity can tie across children spawned in the
        # same tick; identity is the (PID, starttime) pair, and both must remain
        # independently faultable.
        a, b = spawn_sleep(), spawn_sleep()
        faults = recovery.Faults()
        try:
            faults.register('a', a.process)
            faults.register('b', b.process)
            faults.kill('a', a.process)
            faults.kill('b', b.process)
        finally:
            a.close()
            b.close()


class FaultsTests(unittest.TestCase):
    def test_registered_kill_requires_exact_identity(self):
        faults = recovery.Faults()
        role = spawn_sleep()
        try:
            faults.register('victim', role.process)
            faults.kill('victim', role.process)
            self.assertEqual(role.process.returncode, -signal.SIGKILL)
            with self.assertRaises(Failure):
                faults.kill('victim', role.process)  # permit consumed
        finally:
            role.close()

    def test_unregistered_and_mismatched_targets_refused(self):
        faults = recovery.Faults()
        a, b = spawn_sleep(), spawn_sleep()
        try:
            faults.register('a', a.process)
            with self.assertRaises(Failure):
                faults.kill('b', b.process)  # not registered
            with self.assertRaises(Failure):
                faults.kill('a', b.process)  # identity mismatch
            with self.assertRaises(Failure):
                faults.refuse('stray', b.process)  # gate always raises
            self.assertIsNone(b.process.poll())  # refused targets stay alive
            faults.kill('a', a.process)
        finally:
            a.close()
            b.close()

    def test_pid_reuse_detected_via_starttime(self):
        faults = recovery.Faults()
        first = spawn_sleep()
        faults.register('slot', first.process)
        pid = first.process.pid
        first.process.kill()
        first.process.wait(timeout=2)
        second = spawn_sleep()  # may reuse the PID; starttime must differ
        try:
            second.process.pid  # exists; identity check must still refuse
            with self.assertRaises(Failure):
                faults.kill('slot', second.process)
        finally:
            second.close()


class ExpectEofTests(unittest.TestCase):
    def test_eof_observed_and_reaped(self):
        role = spawn_sleep()
        role.process.kill()
        recovery.expect_eof(role, code=-signal.SIGKILL)
        self.assertEqual(role.process.returncode, -signal.SIGKILL)

    def test_eof_times_out_for_live_process(self):
        role = spawn_sleep()
        try:
            with self.assertRaises(Failure):
                recovery.expect_eof(role, code=-signal.SIGKILL)
        finally:
            role.process.kill()
            role.process.wait(timeout=2)
            role.expected_exit = -signal.SIGKILL
            role.close()


if __name__ == '__main__':
    unittest.main()
