#!/usr/bin/env python3
"""Prove live-bundle durability ordering with the real Linux syscall stream."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable


LIVE_TEST = "calibrated_live_run_locks_current_and_reset_boots_into_artifact_bound_outputs"
REPORT_SCHEMA = "invisible-keyboard-live-clock-report-v2"
TRACE_LINE = re.compile(r"^(?P<pid>[0-9]+) +[0-9]+\.[0-9]+ +(?P<body>.*)$")
RESUMED_CALL = re.compile(r"^<\.\.\. (?P<name>[a-z0-9_]+) resumed>(?P<suffix>.*)$")
SUCCESSFUL_WRITE = re.compile(r"= (?P<count>[1-9][0-9]*)$")


class DurabilityProofError(RuntimeError):
    """The real execution did not establish the required durability order."""


@dataclass(frozen=True)
class TraceCall:
    """One completed syscall, including calls split by strace interleaving."""

    pid: int
    start_index: int
    end_index: int
    body: str

    @property
    def name(self) -> str:
        return self.body.split("(", 1)[0]


def required_runfile(setting_name: str) -> Path:
    runfiles_root = os.environ.get("TEST_SRCDIR")
    relative_path = os.environ.get(setting_name)
    if runfiles_root is None or relative_path is None:
        raise DurabilityProofError(f"missing Bazel runfile setting: {setting_name}")
    path = Path(runfiles_root) / relative_path
    if not path.is_file():
        raise DurabilityProofError(f"missing Bazel runfile: {path}")
    return path


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb", buffering=0) as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def parse_trace(path: Path) -> list[TraceCall]:
    """Join strace's unfinished/resumed pairs without losing completion order."""
    calls: list[TraceCall] = []
    pending: dict[int, tuple[int, str, str]] = {}
    for index, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines()):
        matched = TRACE_LINE.match(raw_line)
        if matched is None:
            continue
        pid = int(matched.group("pid"))
        body = matched.group("body")
        if body.endswith(" <unfinished ...>"):
            prefix = body.removesuffix(" <unfinished ...>")
            name = prefix.split("(", 1)[0]
            if pid in pending:
                raise DurabilityProofError(f"trace has nested unfinished calls for TID {pid}")
            pending[pid] = (index, name, prefix)
            continue
        resumed = RESUMED_CALL.match(body)
        if resumed is not None:
            if pid not in pending:
                raise DurabilityProofError(f"trace resumes an unknown call for TID {pid}")
            start_index, name, prefix = pending.pop(pid)
            if name != resumed.group("name"):
                raise DurabilityProofError(
                    f"trace resumes {resumed.group('name')} over pending {name} for TID {pid}"
                )
            calls.append(
                TraceCall(
                    pid=pid,
                    start_index=start_index,
                    end_index=index,
                    body=prefix + resumed.group("suffix"),
                )
            )
            continue
        if "(" in body:
            calls.append(TraceCall(pid=pid, start_index=index, end_index=index, body=body))
    if pending:
        raise DurabilityProofError(f"trace ended with unfinished calls for TIDs {sorted(pending)}")
    return calls


def require_call(
    calls: list[TraceCall],
    predicate: Callable[[TraceCall], bool],
    *,
    label: str,
    after: int = -1,
    pid: int | None = None,
) -> TraceCall:
    for call in calls:
        if call.start_index <= after or (pid is not None and call.pid != pid):
            continue
        if predicate(call):
            return call
    raise DurabilityProofError(f"missing {label} after trace event {after}")


def successful_write_to(path: Path, marker: str) -> Callable[[TraceCall], bool]:
    descriptor = f"<{path}>"

    def matches(call: TraceCall) -> bool:
        matched = SUCCESSFUL_WRITE.search(call.body)
        return (
            call.name in {"write", "writev"}
            and descriptor in call.body
            and marker in call.body
            and matched is not None
            and int(matched.group("count")) > 0
        )

    return matches


def successful_stdout_write(marker: str) -> Callable[[TraceCall], bool]:
    def matches(call: TraceCall) -> bool:
        return (
            call.name in {"write", "writev"}
            and call.body.startswith(("write(1<", "writev(1<"))
            and marker in call.body
            and SUCCESSFUL_WRITE.search(call.body) is not None
        )

    return matches


def successful_fsync(path: Path) -> Callable[[TraceCall], bool]:
    descriptor = f"<{path}>"
    return lambda call: (
        call.name in {"fsync", "fdatasync"}
        and descriptor in call.body
        and call.body.endswith("= 0")
    )


def exactly_one(directory: Path, pattern: str) -> Path:
    matches = list(directory.glob(pattern))
    if len(matches) != 1:
        raise DurabilityProofError(
            f"expected one {pattern} output in {directory}, found {len(matches)}"
        )
    return matches[0].resolve()


def verify_one_bundle(
    calls: list[TraceCall], directory: Path, label: str
) -> dict[str, object]:
    session = exactly_one(directory, f"{label}-live-*.iks")
    manifest = session.with_suffix(".manifest.json")
    report = session.with_suffix(".live-clock-report.json")
    for path in (manifest, report):
        if not path.is_file():
            raise DurabilityProofError(f"missing live-bundle member: {path}")

    session_bytes = session.read_bytes()
    if b"END3" not in session_bytes[-64:]:
        raise DurabilityProofError(f"{session.name} lacks its terminal END3 footer")
    report_value = json.loads(report.read_bytes())
    session_sha256 = hashlib.sha256(session_bytes).hexdigest()
    manifest_sha256 = sha256_file(manifest)
    if report_value.get("schema") != REPORT_SCHEMA:
        raise DurabilityProofError(f"{report.name} does not use {REPORT_SCHEMA}")
    if report_value.get("session_sha256") != session_sha256:
        raise DurabilityProofError(f"{report.name} does not bind the exact session bytes")
    if report_value.get("acquisition_manifest_sha256") != manifest_sha256:
        raise DurabilityProofError(f"{report.name} does not bind its runtime manifest")

    footer = require_call(
        calls,
        successful_write_to(session, "END3"),
        label=f"{label} END3 write",
    )
    pid = footer.pid
    manifest_sync = require_call(
        calls,
        successful_fsync(manifest),
        label=f"{label} manifest fsync",
        pid=pid,
    )
    manifest_parent_sync = require_call(
        calls,
        successful_fsync(directory.resolve()),
        label=f"{label} manifest parent-directory fsync",
        after=manifest_sync.end_index,
        pid=pid,
    )
    if manifest_parent_sync.end_index >= footer.start_index:
        raise DurabilityProofError(f"{label} session footer preceded durable manifest publication")

    session_sync = require_call(
        calls,
        successful_fsync(session),
        label=f"{label} session fsync",
        after=footer.end_index,
        pid=pid,
    )
    session_parent_sync = require_call(
        calls,
        successful_fsync(directory.resolve()),
        label=f"{label} session parent-directory fsync",
        after=session_sync.end_index,
        pid=pid,
    )
    report_sync = require_call(
        calls,
        successful_fsync(report),
        label=f"{label} report fsync",
        after=session_parent_sync.end_index,
        pid=pid,
    )
    report_parent_sync = require_call(
        calls,
        successful_fsync(directory.resolve()),
        label=f"{label} report parent-directory fsync",
        after=report_sync.end_index,
        pid=pid,
    )
    neutral_status = require_call(
        calls,
        successful_stdout_write(r"shutdown_neutralization=confirmed\n"),
        label=f"{label} neutralization status",
        after=report_parent_sync.end_index,
        pid=pid,
    )
    durable_status = require_call(
        calls,
        successful_stdout_write(r"evidence_finalization=durable\n"),
        label=f"{label} durable status",
        after=neutral_status.end_index,
        pid=pid,
    )

    return {
        "pid": pid,
        "manifest": manifest.name,
        "manifest_sha256": manifest_sha256,
        "session": session.name,
        "session_sha256": session_sha256,
        "report": report.name,
        "report_sha256": sha256_file(report),
        "manifest_fsync_completed_at_trace_event": manifest_sync.end_index,
        "manifest_parent_fsync_completed_at_trace_event": manifest_parent_sync.end_index,
        "footer_write_completed_at_trace_event": footer.end_index,
        "session_fsync_completed_at_trace_event": session_sync.end_index,
        "session_parent_fsync_completed_at_trace_event": session_parent_sync.end_index,
        "report_fsync_completed_at_trace_event": report_sync.end_index,
        "report_parent_fsync_completed_at_trace_event": report_parent_sync.end_index,
        "neutral_status_started_at_trace_event": neutral_status.start_index,
        "durable_status_started_at_trace_event": durable_status.start_index,
    }


def main() -> int:
    if sys.platform != "linux":
        raise DurabilityProofError("the syscall durability oracle is Linux-only")
    strace = shutil.which("strace")
    if strace is None:
        raise DurabilityProofError("strace is required for the syscall durability oracle")
    cli_test = required_runfile("IKB_DURABILITY_CLI_TEST_RLOCATION")
    cli_binary = required_runfile("INVISIBLE_KEYBOARD_RLOCATION")
    test_tmpdir = os.environ.get("TEST_TMPDIR")
    if test_tmpdir is None:
        raise DurabilityProofError("Bazel did not supply TEST_TMPDIR")
    evidence_directory = Path(test_tmpdir) / "live-durability-syscalls"
    evidence_directory.mkdir()
    trace = evidence_directory / "trace.log"

    environment = os.environ.copy()
    environment["TEST_TMPDIR"] = str(evidence_directory)
    environment["RUST_TEST_THREADS"] = "1"
    command = [
        strace,
        "-f",
        "-qq",
        "-ttt",
        "-yy",
        "-s",
        "65536",
        "-e",
        "trace=execve,openat,write,writev,fsync,fdatasync,close",
        "-o",
        str(trace),
        str(cli_test),
        "--exact",
        LIVE_TEST,
        "--test-threads=1",
        "--nocapture",
    ]
    completed = subprocess.run(
        command,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=120,
        check=False,
    )
    if completed.returncode != 0:
        raise DurabilityProofError(
            "traced live contract failed\n"
            f"stdout:\n{completed.stdout.decode(errors='replace')}\n"
            f"stderr:\n{completed.stderr.decode(errors='replace')}"
        )

    calls = parse_trace(trace)
    bundles = {
        label: verify_one_bundle(calls, evidence_directory, label)
        for label in ("current", "reset")
    }
    receipt = {
        "schema": "invisible-keyboard-linux-durability-syscall-proof-v1",
        "claim_scope": (
            "Linux syscall order for two real recording-mode CLI shutdowns; fsync semantics "
            "do not prove storage hardware beyond the operating-system contract"
        ),
        "test": LIVE_TEST,
        "cli_sha256": sha256_file(cli_binary),
        "cli_test_sha256": sha256_file(cli_test),
        "strace_version": subprocess.run(
            [strace, "--version"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=True,
        ).stdout.splitlines()[0],
        "trace_sha256": sha256_file(trace),
        "bundles": bundles,
    }
    receipt_path = evidence_directory / "receipt.json"
    receipt_path.write_text(
        json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )

    undeclared_outputs = os.environ.get("TEST_UNDECLARED_OUTPUTS_DIR")
    if undeclared_outputs is not None:
        output_directory = Path(undeclared_outputs)
        shutil.copy2(trace, output_directory / "live-durability-strace.log")
        shutil.copy2(receipt_path, output_directory / "live-durability-receipt.json")
    print(json.dumps(receipt, sort_keys=True, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (DurabilityProofError, OSError, ValueError, json.JSONDecodeError) as error:
        print(f"durability syscall proof failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
