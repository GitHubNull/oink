#!/usr/bin/env python3
"""Phase 5 FUSE gate: the product file_share bridge mounting a scripted fake
Namespace (phase5_fuse_probe) and serving it back through the kernel.

No X session is needed. Build the probe first:

  cargo build --locked -p oink-clipboard --example phase5_fuse_probe

The gate always runs --self-check (no mount needed), then a real run. Both of
these are PASS for the real run:
- full evidence: mounted -> entries -> per-file SHA-256 ok -> safe symlink ->
  guarded escape link -> counters -> done failures:0 -> unmounted
- a clean block report: {"event":"blocked","reason": ...NoNewPrivs...} with
  exit 0 (the agent sandbox, where setuid fusermount3 refuses to run)

--stale and --offline additionally prove injected failures surface as read
errors instead of fabricated EOF. Payloads are synthetic; only lengths and
digests are checked.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from linux_dnd_protocol import Failure, check

HELLO = b"oink phase5 fuse hello"
A_TEXT = b"folder a" * 64
B_BIN = bytes([7]) * 7
LINK = "hello.txt"


def pattern(length):
    return bytes(ord('a') + index % 26 for index in range(length))


def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


def expected_files(big_kib):
    """Exactly what the probe's fake serves; the probe reports flat names."""
    return {
        "hello.txt": (len(HELLO), sha256_hex(HELLO)),
        "a.txt": (len(A_TEXT), sha256_hex(A_TEXT)),
        "big.bin": (big_kib * 1024, sha256_hex(pattern(big_kib * 1024))),
    }


def expected_entries(big_kib):
    """(name, kind, size) multiset the walk must report."""
    entries = [
        ("hello.txt", "file", len(HELLO)),
        ("folder", "dir", 0),
        ("deep", "dir", 0),
        ("big.bin", "file", big_kib * 1024),
        ("link", "symlink", len(LINK)),
        ("escape", "symlink", len("/etc/passwd")),
        # Both duplicates stay listable; lookup deterministically resolves
        # the first, so both report the first entry's size.
        ("dup.txt", "file", 3),
        ("dup.txt", "file", 3),
        ("a.txt", "file", len(A_TEXT)),
        ("b.bin", "file", len(B_BIN)),
    ]
    entries += [(f"d{index}.txt", "file", 3) for index in range(5)]
    return sorted(entries)


def summarize(events):
    """Fold probe JSON lines into the outcome the gate asserts on."""
    summary = {"status": None, "reason": None, "failures": None, "files": {},
               "entries": [], "links": {}, "guards": [], "read_errors": [],
               "counters": None, "mount": None, "unmount": None,
               "self_check": []}
    for event in events:
        name = event.get("event")
        if not isinstance(name, str):
            raise Failure(f"malformed probe event: {event}")
        if name == "mounted":
            summary["status"] = "mounted"
            summary["mount"] = event.get("path")
        elif name == "blocked":
            summary["status"] = "blocked"
            summary["reason"] = event.get("reason")
        elif name == "error":
            raise Failure(f"probe error: {event}")
        elif name == "entry":
            summary["entries"].append((event["name"], event["kind"], event["size"]))
        elif name == "file":
            summary["files"].setdefault(event["name"], []).append({
                "length": event["length"], "sha256": event["sha256"],
                "ok": event["ok"]})
        elif name == "symlink":
            summary["links"][event["name"]] = event.get("target")
        elif name == "symlink-guard":
            summary["guards"].append(event)
        elif name == "read-error":
            summary["read_errors"].append(event)
        elif name == "counters":
            summary["counters"] = {key: event[key] for key in ("lists", "opens")}
        elif name == "self-check":
            summary["self_check"].append(event)
        elif name == "done":
            summary["failures"] = event["failures"]
        elif name == "unmounted":
            summary["unmount"] = "unmounted"
        elif name == "unmount-leftover":
            summary["unmount"] = "unmount-leftover"
    summary["entries"].sort()
    return summary


def check_mount_evidence(summary, big_kib, expect_failures):
    """Assert the full mounted run; returns a list of evidence strings."""
    check(summary["status"] == "mounted", f'expected a mount, got {summary["status"]}')
    check(isinstance(summary["mount"], str) and "/oink/mounts/" in summary["mount"],
          f'mount must live under oink/mounts: {summary}')
    got_entries = summary["entries"]
    check(got_entries == expected_entries(big_kib),
          f'entries mismatch:\nwant {expected_entries(big_kib)}\ngot  {got_entries}')
    evidence = []
    if expect_failures:
        check(summary["read_errors"], "injected failure produced no read errors")
        for error in summary["read_errors"]:
            check(error["expected"] is True, f'unexpected read error: {error}')
        evidence.append(f"read-errors={len(summary['read_errors'])}")
    else:
        check(not summary["read_errors"],
              f'unexpected read errors: {summary["read_errors"]}')
        for name, (length, digest) in expected_files(big_kib).items():
            got = summary["files"].get(name)
            check(got and len(got) == 1, f'missing file evidence for {name}')
            entry = got[0]
            check(entry["ok"] is True, f'{name} digest mismatch: {entry}')
            check(entry["length"] == length and entry["sha256"] == digest,
                  f'{name}: want {(length, digest)}, got {entry}')
            evidence.append(f'{name}={length}:{digest[:12]}')
    safe = [target for name, target in summary["links"].items()
            if name.endswith("/link")]
    check(safe == [LINK], f'safe link: {summary["links"]}')
    check(summary["guards"], "escape symlink was not guarded")
    for guard in summary["guards"]:
        check(guard["safe"] is False, f'escape link must be refused: {guard}')
    check(summary["failures"] == 0, f'probe reported failures: {summary["failures"]}')
    check(summary["unmount"] == "unmounted", f'unmount state: {summary["unmount"]}')
    check(summary["counters"] and summary["counters"]["lists"] > 0,
          f'missing namespace counters: {summary["counters"]}')
    return evidence


def check_self_check(summary):
    checks = summary["self_check"]
    check(len(checks) == 2, f'self-check events: {checks}')
    check(checks[1]["deep_entries"] == 5 and checks[1]["lists"] == 3,
          f'pagination self-check: {checks[1]}')
    digests = summary["files"].get("self-check") or []
    check(len(digests) == 2 and all(entry["ok"] for entry in digests),
          f'self-check digests: {digests}')


def check_block_report(summary):
    check(summary["status"] == "blocked", f'expected blocked, got {summary}')
    reason = summary["reason"] or ""
    check("NoNewPrivs" in reason, f'block reason must name NoNewPrivs: {reason!r}')
    return [f"blocked={reason[:48]}"]


def run_probe(probe, args, runtime):
    env = dict(os.environ)
    env["XDG_RUNTIME_DIR"] = str(runtime)
    env["TMPDIR"] = str(runtime)
    try:
        completed = subprocess.run(
            [probe, *args], capture_output=True, text=True, timeout=120, env=env)
    except subprocess.TimeoutExpired as expired:
        raise Failure(f"probe timed out: {expired}") from expired
    events = []
    for line in completed.stdout.splitlines():
        line = line.strip()
        if line.startswith("{"):
            events.append(json.loads(line))
    return events, completed.returncode, completed.stderr


def scenario(probe, big_kib=64, workdir=None):
    work = Path(workdir) if workdir else Path(
        tempfile.mkdtemp(prefix="oink-fuse-gate-"))
    runtime = work / "runtime"
    runtime.mkdir(parents=True, exist_ok=True)
    try:
        events, code, stderr = run_probe(probe, ["--self-check"], runtime)
        check(code == 0, f"self-check exit {code}: {stderr.strip()[-400:]}")
        check_self_check(summarize(events))
        print("PASS fuse-self-check roots=8 deep=5/3lists digests=ok")
        outcomes = []
        for mode in ("normal", "--stale", "--offline"):
            args = [mode] if mode.startswith("--") else ["--big", str(big_kib)]
            events, code, stderr = run_probe(probe, args, runtime)
            check(code == 0, f"probe {args} exit {code}: {stderr.strip()[-400:]}")
            check(events, f"probe {args} produced no events")
            summary = summarize(events)
            if summary["status"] == "blocked":
                outcomes = [check_block_report(summary)]
                print(f"PASS fuse-blocked {'; '.join(outcomes)}")
                return "blocked"
            outcomes = check_mount_evidence(
                summary, big_kib, expect_failures=mode in ("--stale", "--offline"))
            leftovers = sorted(
                str(path.relative_to(runtime))
                for path in runtime.rglob("*")
                if (path.is_file() or (path.is_dir() and any(path.iterdir())))
                and path != runtime / "oink")
            check(not leftovers, f"mount leftovers in runtime dir: {leftovers}")
            label = "stale" if mode == "--stale" else "offline" if mode == "--offline" else "mounted"
            print(f"PASS fuse-{label} {'; '.join(outcomes)}")
        return "mounted"
    finally:
        if workdir is None:
            shutil.rmtree(work, ignore_errors=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", default=None, help="path to phase5_fuse_probe")
    parser.add_argument("--big", type=int, default=64, help="big.bin size in KiB")
    parser.add_argument("--workdir", default=None, help="keep artifacts here")
    args = parser.parse_args(argv)
    probe = args.probe or str(
        Path(__file__).resolve().parent.parent
        / "target/debug/examples/phase5_fuse_probe")
    if not Path(probe).exists():
        print(f"probe not built: {probe}", file=sys.stderr)
        print("cargo build --locked -p oink-clipboard --example phase5_fuse_probe",
              file=sys.stderr)
        return 2
    try:
        scenario(probe, args.big, args.workdir)
    except Failure as failure:
        print(f"FAIL {failure}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
