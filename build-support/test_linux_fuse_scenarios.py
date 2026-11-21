"""Pure unit tests for the Phase 5 FUSE gate: canned probe JSON only, never
mounts anything."""
import unittest

import linux_dnd_protocol as protocol
import linux_phase5_fuse_scenarios as fuse_gate


BIG_KIB = 64


def mounted_events(fail_reads=False):
    files = fuse_gate.expected_files(BIG_KIB)
    events = [{"event": "mounted", "path": "/run/user/1000/oink/mounts/oink-1-0"}]
    for name, kind, size in fuse_gate.expected_entries(BIG_KIB):
        events.append({"event": "entry", "name": name, "kind": kind, "size": size})
    if fail_reads:
        for name in files:
            events.append({"event": "read-error", "name": f"/mnt/{name}",
                           "errno": 5, "reason": "Input/output error",
                           "expected": True})
    else:
        for name, (length, digest) in files.items():
            events.append({"event": "file", "name": name, "length": length,
                           "sha256": digest, "ok": True})
    events.append({"event": "symlink", "name": "/mnt/link",
                   "target": fuse_gate.LINK})
    events.append({"event": "symlink-guard", "name": "/mnt/escape",
                   "result": "Err(..)", "safe": False, "expected": False})
    events.append({"event": "counters", "lists": 22, "opens": 3})
    events.append({"event": "done", "failures": 0})
    events.append({"event": "unmounted",
                   "path": "/run/user/1000/oink/mounts/oink-1-0"})
    return events


def self_check_events():
    return [
        {"event": "self-check", "roots": 8},
        {"event": "self-check", "deep_entries": 5, "lists": 3},
        {"event": "file", "name": "self-check", "length": 22,
         "sha256": fuse_gate.sha256_hex(fuse_gate.HELLO), "ok": True},
        {"event": "file", "name": "self-check", "length": BIG_KIB * 1024,
         "sha256": fuse_gate.sha256_hex(fuse_gate.pattern(BIG_KIB * 1024)),
         "ok": True},
        {"event": "done", "failures": 0},
    ]


class SummarizeTests(unittest.TestCase):
    def test_mounted_run_folds_into_structured_summary(self):
        summary = fuse_gate.summarize(mounted_events())
        self.assertEqual(summary["status"], "mounted")
        self.assertEqual(summary["failures"], 0)
        self.assertEqual(summary["unmount"], "unmounted")
        self.assertEqual(summary["links"]["/mnt/link"], fuse_gate.LINK)
        self.assertEqual(len(summary["guards"]), 1)
        self.assertEqual(summary["entries"], fuse_gate.expected_entries(BIG_KIB))
        self.assertEqual(summary["files"]["hello.txt"][0]["sha256"],
                         fuse_gate.sha256_hex(fuse_gate.HELLO))

    def test_blocked_run_reports_reason(self):
        summary = fuse_gate.summarize(
            [{"event": "blocked", "reason": "NoNewPrivs=1 blocks setuid fusermount3"}])
        self.assertEqual(summary["status"], "blocked")
        self.assertIn("NoNewPrivs", summary["reason"])

    def test_malformed_event_is_rejected(self):
        with self.assertRaises(protocol.Failure):
            fuse_gate.summarize([{"not-event": 1}])

    def test_probe_error_event_fails_the_gate(self):
        with self.assertRaises(protocol.Failure):
            fuse_gate.summarize([{"event": "error", "reason": "boom"}])


class EvidenceTests(unittest.TestCase):
    def test_full_evidence_passes_and_reports_lengths(self):
        summary = fuse_gate.summarize(mounted_events())
        evidence = fuse_gate.check_mount_evidence(summary, BIG_KIB, False)
        self.assertEqual(len(evidence), 3)
        self.assertIn("hello.txt=22:", evidence[0])

    def test_injected_failures_pass_as_read_errors(self):
        summary = fuse_gate.summarize(mounted_events(fail_reads=True))
        evidence = fuse_gate.check_mount_evidence(summary, BIG_KIB, True)
        self.assertEqual(evidence, ["read-errors=3"])

    def test_unexpected_read_error_fails_normal_run(self):
        events = mounted_events() + [
            {"event": "read-error", "name": "/mnt/x", "errno": 5,
             "reason": "io", "expected": True}]
        summary = fuse_gate.summarize(events)
        with self.assertRaises(protocol.Failure):
            fuse_gate.check_mount_evidence(summary, BIG_KIB, False)

    def test_missing_read_errors_fail_injected_run(self):
        summary = fuse_gate.summarize(mounted_events())
        with self.assertRaises(protocol.Failure):
            fuse_gate.check_mount_evidence(summary, BIG_KIB, True)

    def test_digest_mismatch_fails(self):
        events = mounted_events()
        for event in events:
            if event.get("event") == "file" and event["name"] == "hello.txt":
                event["ok"] = False
        summary = fuse_gate.summarize(events)
        with self.assertRaises(protocol.Failure):
            fuse_gate.check_mount_evidence(summary, BIG_KIB, False)

    def test_entry_mismatch_fails(self):
        events = mounted_events()
        events = [event for event in events
                  if not (event.get("event") == "entry"
                          and event.get("name") == "big.bin")]
        summary = fuse_gate.summarize(events)
        with self.assertRaises(protocol.Failure):
            fuse_gate.check_mount_evidence(summary, BIG_KIB, False)

    def test_leftover_mount_fails(self):
        events = mounted_events()[:-1] + [
            {"event": "unmount-leftover",
             "path": "/run/user/1000/oink/mounts/oink-1-0"}]
        summary = fuse_gate.summarize(events)
        with self.assertRaises(protocol.Failure):
            fuse_gate.check_mount_evidence(summary, BIG_KIB, False)

    def test_mount_must_live_under_oink_mounts(self):
        events = mounted_events()
        events[0]["path"] = "/tmp/oink-elsewhere"
        summary = fuse_gate.summarize(events)
        with self.assertRaises(protocol.Failure):
            fuse_gate.check_mount_evidence(summary, BIG_KIB, False)

    def test_unguarded_escape_link_fails(self):
        events = [event for event in mounted_events()
                  if event.get("event") != "symlink-guard"]
        summary = fuse_gate.summarize(events)
        with self.assertRaises(protocol.Failure):
            fuse_gate.check_mount_evidence(summary, BIG_KIB, False)

    def test_block_report_requires_noprivs_reason(self):
        summary = fuse_gate.summarize([{"event": "blocked", "reason": None}])
        with self.assertRaises(protocol.Failure):
            fuse_gate.check_block_report(summary)
        summary = fuse_gate.summarize(
            [{"event": "blocked", "reason": "NoNewPrivs=1 set"}])
        self.assertEqual(fuse_gate.check_block_report(summary),
                         ["blocked=NoNewPrivs=1 set"])

    def test_self_check_evidence(self):
        fuse_gate.check_self_check(fuse_gate.summarize(self_check_events()))
        broken = self_check_events()
        broken[1]["lists"] = 2
        with self.assertRaises(protocol.Failure):
            fuse_gate.check_self_check(fuse_gate.summarize(broken))

    def test_expected_digests_track_big_size(self):
        small = fuse_gate.expected_files(1)["big.bin"]
        large = fuse_gate.expected_files(64)["big.bin"]
        self.assertEqual(small[0], 1024)
        self.assertEqual(large[0], 64 * 1024)
        self.assertNotEqual(small[1], large[1])


if __name__ == "__main__":
    unittest.main()
