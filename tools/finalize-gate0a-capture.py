#!/usr/bin/env python3
"""Finalize and score one Gate 0A capture inside a Crabbox.

The finalizer verifies the hardware handoff, hashes IKS3 and raw evidence,
creates the reducer-input sidecar, invokes the production scorer exactly once,
and publishes the sidecar and report as one no-replace pair.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Final, Sequence


HANDOFF_SCHEMA: Final = "invisible-keyboard-gate0-dslogic-handoff-v2"
SIDECAR_SCHEMA: Final = "invisible-keyboard-gate0-dslogic-capture-v1"
SAMPLE_RATE_HZ: Final = 2_000_000
REQUESTED_SAMPLES: Final = 7_220_000_000
RAW_WORD_BYTES: Final = 2
EXPECTED_RAW_BYTES: Final = REQUESTED_SAMPLES * RAW_WORD_BYTES
STOP_WINDOW_FIRST_BYTE: Final = 14_416_000_000
STOP_WINDOW_LAST_BYTE: Final = 14_417_000_000
MARKER_FALL_DEADLINE_SAMPLE: Final = 7_210_000_000
MAX_RAW_POLL_INTERVAL_NS: Final = 10_000_000
DRIVER_BASE_COMMIT: Final = "0bc2487778e660f4d3116729b6f4aee2b1996bb0"
SIGROK_CLI_BASE_COMMIT: Final = "f44dd91347e7ac797cefc23162b9fcf0b7329f1f"
DEVICE_MODEL: Final = "DSLogic Plus"
CHANNELS: Final = "0,1,2,3,4,5,6,7,8,9"
MAX_JSON_BYTES: Final = 16 * 1024 * 1024
MAX_LOG_LINE_BYTES: Final = 4096
MAX_COMMAND_ARGUMENT_BYTES: Final = 4096
SCORER_TIMEOUT_SECONDS: Final = 3600
SCORER_POLL_INTERVAL_SECONDS: Final = 0.1
MAX_SCORER_STDERR_BYTES: Final = 8 * 1024 * 1024
SCORER_COPY_BYTES: Final = 1024 * 1024
SCORER_MEMFD_NAME: Final = "gate0a-scorer"
U64_MAX: Final = 0xFFFF_FFFF_FFFF_FFFF
U32_MAX: Final = 0xFFFF_FFFF
REPORT_SCHEMA: Final = "invisible-keyboard-gate0a-report-v6"
TRACE_SCHEMA: Final = "invisible-keyboard-gate0-dslogic-capture-v1"
SHA256 = re.compile(r"[0-9a-f]{64}\Z")
DRIVER_RECEIPT = re.compile(
    rb"IKB_DSLOGIC_TRANSFER_V1 completed_transfers=([1-9][0-9]*) anomalies=0"
)
CAPTURE_RECEIPT = re.compile(
    rb"IKB_GATE0_CAPTURE_V2 samples=7220000000 "
    rb"completed_transfers=([1-9][0-9]*) anomalies=0 "
    rb"stop_observed_raw_bytes=([0-9]+) "
    rb"session_bytes=([0-9]+) "
    rb"locked_manifest_sha256=([0-9a-f]{64}) "
    rb"firmware_bundle_sha256=([0-9a-f]{64})\n\Z"
)
CAPTURE_FAILURE_MARKERS: Final = (
    b"IKB_DSLOGIC_TRANSFER_FAILURE_V1",
    b"Capture rejected",
    b"CRITICAL",
    b"WARNING",
)
FIRMWARE_INVENTORY: Final = (
    {
        "name": "dreamsourcelab-dslogic-plus-fpga.fw",
        "bytes": 341_160,
        "sha256": "e16c70e4af9181044e3ec3a363e4bb109f1d731b4f9f046f8bc1d1015a7842a7",
    },
    {
        "name": "dreamsourcelab-dslogic-plus-fx2.fw",
        "bytes": 8_120,
        "sha256": "9197e08dd33a9cda61cc8cd3de559cbae9b049b2dfa827c089ffe0528f5dcc5e",
    },
)


class FinalizeError(RuntimeError):
    """A fail-closed handoff, artifact, scorer, or publication error."""


def require_regular_file(path: Path, *, executable: bool = False) -> os.stat_result:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise FinalizeError(f"cannot inspect {path}: {error}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise FinalizeError(f"expected a regular, non-symlink file: {path}")
    if executable and metadata.st_mode & 0o111 == 0:
        raise FinalizeError(f"executable input has no execute bit: {path}")
    return metadata


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb", buffering=0) as source:
        while chunk := source.read(8 * 1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_descriptor(descriptor: int) -> str:
    digest = hashlib.sha256()
    os.lseek(descriptor, 0, os.SEEK_SET)
    while chunk := os.read(descriptor, 8 * 1024 * 1024):
        digest.update(chunk)
    os.lseek(descriptor, 0, os.SEEK_SET)
    return digest.hexdigest()


def admit_scorer_image(
    scorer: Path, *, expected_bytes: int, expected_sha256: str
) -> int:
    """Copy, seal, and hash the exact executable image retained for exec."""

    source_fd = -1
    image_fd = -1
    try:
        source_fd = os.open(
            scorer,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW,
        )
        source_metadata = os.fstat(source_fd)
        if (
            not stat.S_ISREG(source_metadata.st_mode)
            or source_metadata.st_mode & 0o111 == 0
            or source_metadata.st_size == 0
            or source_metadata.st_size != expected_bytes
        ):
            raise FinalizeError(
                "scorer bytes must equal the wrapper-published recorder binary"
            )

        try:
            image_fd = os.memfd_create(
                SCORER_MEMFD_NAME,
                os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING,
            )
            required_seals = (
                fcntl.F_SEAL_WRITE
                | fcntl.F_SEAL_GROW
                | fcntl.F_SEAL_SHRINK
                | fcntl.F_SEAL_SEAL
            )
        except AttributeError as error:
            raise FinalizeError(
                "Crabbox Linux lacks sealed memfd scorer execution"
            ) from error

        copied_bytes = 0
        while chunk := os.read(source_fd, SCORER_COPY_BYTES):
            copied_bytes += len(chunk)
            remaining = memoryview(chunk)
            while remaining:
                written = os.write(image_fd, remaining)
                if written == 0:
                    raise FinalizeError("sealed scorer image accepted a short write")
                remaining = remaining[written:]
        if copied_bytes != expected_bytes:
            raise FinalizeError(
                "scorer bytes must equal the wrapper-published recorder binary"
            )

        os.fchmod(image_fd, 0o500)
        fcntl.fcntl(image_fd, fcntl.F_ADD_SEALS, required_seals)
        if fcntl.fcntl(image_fd, fcntl.F_GET_SEALS) & required_seals != required_seals:
            raise FinalizeError("scorer image did not retain all required seals")
        if sha256_descriptor(image_fd) != expected_sha256:
            raise FinalizeError(
                "scorer bytes must equal the wrapper-published recorder binary"
            )

        admitted_fd = image_fd
        image_fd = -1
        return admitted_fd
    except OSError as error:
        raise FinalizeError(f"cannot admit scorer executable: {error}") from error
    finally:
        if source_fd >= 0:
            os.close(source_fd)
        if image_fd >= 0:
            os.close(image_fd)


def fsync_directory(directory: Path) -> None:
    descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def sync_regular_file(path: Path) -> int:
    metadata = require_regular_file(path)
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    return metadata.st_size


def verify_iks3_envelope(path: Path) -> int:
    """Reject an unfinalized session before hashing or invoking Rust audit."""

    metadata = require_regular_file(path)
    if metadata.st_size < 80:
        raise FinalizeError("IKS3 session is too short")
    with path.open("rb", buffering=0) as session:
        header = session.read(8)
        session.seek(-16, os.SEEK_END)
        footer = session.read(4)
    if header != b"IKS3\x03\x00\x00\x00" or footer != b"END3":
        raise FinalizeError("session lacks the canonical IKS3 header or END3 footer")
    return metadata.st_size


def require_exact_keys(value: object, expected: set[str], label: str) -> dict[str, object]:
    if not isinstance(value, dict) or set(value) != expected:
        raise FinalizeError(f"{label} fields do not match the canonical schema")
    return value


def require_int(value: object, expected: int | None, label: str) -> int:
    if (
        isinstance(value, bool)
        or not isinstance(value, int)
        or value < 0
        or value > U64_MAX
    ):
        raise FinalizeError(f"{label} must be a uint64")
    if expected is not None and value != expected:
        raise FinalizeError(f"{label} is {value}; expected {expected}")
    return value


def require_optional_int(value: object, maximum: int, label: str) -> int | None:
    if value is None:
        return None
    return require_bounded_int(value, maximum, label)


def require_bounded_int(value: object, maximum: int, label: str) -> int:
    parsed = require_int(value, None, label)
    if parsed > maximum:
        raise FinalizeError(f"{label} exceeds {maximum}")
    return parsed


def require_string(value: object, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise FinalizeError(f"{label} must be a non-empty string")
    if len(value.encode("utf-8")) > MAX_COMMAND_ARGUMENT_BYTES:
        raise FinalizeError(f"{label} exceeds {MAX_COMMAND_ARGUMENT_BYTES} bytes")
    return value


def require_sha256(value: object, label: str) -> str:
    digest = require_string(value, label)
    if not SHA256.fullmatch(digest):
        raise FinalizeError(f"{label} is not lowercase SHA-256")
    return digest


def resolve_bound_path(directory: Path, value: object, label: str) -> Path:
    text = require_string(value, label)
    if text in {".", ".."} or Path(text).name != text or "/" in text or "\\" in text:
        raise FinalizeError(f"{label} must be a safe relative basename")
    path = directory / text
    require_regular_file(path)
    return path


def validate_bound_artifact(
    value: object, directory: Path, label: str, *, hash_required: bool
) -> tuple[Path, dict[str, object]]:
    keys = {"path", "bytes", "sha256"} if hash_required else {"path", "bytes"}
    artifact = require_exact_keys(value, keys, label)
    path = resolve_bound_path(directory, artifact["path"], f"{label}.path")
    byte_count = require_int(artifact["bytes"], path.stat().st_size, f"{label}.bytes")
    result: dict[str, object] = {"path": path.name, "bytes": byte_count}
    if hash_required:
        expected_hash = require_sha256(artifact["sha256"], f"{label}.sha256")
        actual_hash = sha256_file(path)
        if actual_hash != expected_hash:
            raise FinalizeError(f"{label} hash does not match the hardware handoff")
        result["sha256"] = actual_hash
    return path, result


def read_bounded_json(path: Path, label: str) -> object:
    metadata = require_regular_file(path)
    if metadata.st_size > MAX_JSON_BYTES:
        raise FinalizeError(f"{label} exceeds {MAX_JSON_BYTES} bytes")
    try:
        return json.loads(path.read_bytes())
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FinalizeError(f"invalid {label} JSON: {error}") from error


def read_handoff(path: Path) -> dict[str, object]:
    value = read_bounded_json(path, "handoff")
    return require_exact_keys(
        value,
        {
            "schema",
            "raw_capture",
            "session_capture",
            "locked_manifest",
            "word_bytes",
            "byte_order",
            "sample_rate_hz",
            "channel_map",
            "voltage_threshold_low_mv",
            "voltage_threshold_high_mv",
            "requested_samples",
            "stop_window_first_byte",
            "stop_window_last_byte",
            "stop_observed_raw_bytes",
            "maximum_raw_poll_interval_ns",
            "capture_started_unix_ms",
            "capture_finished_unix_ms",
            "capture_binary",
            "recorder_binary",
            "capture_log",
            "recorder_log",
            "device_firmware_bundle",
            "acquisition_template",
            "rig_evidence",
            "axis_characterization",
            "driver_base_commit",
            "sigrok_cli_base_commit",
            "device_model",
            "driver_patch",
            "capture_exit_status",
            "recorder_exit_status",
            "loaded_firmware",
            "setup_command",
            "capture_command",
            "recorder_command",
        },
        "handoff",
    )


def validate_channel_map(value: object) -> dict[str, object]:
    channel_map = require_exact_keys(value, {"int1", "d8", "d9", "sda", "scl"}, "channel_map")
    expected = {"int1": [0, 1, 2, 3, 4, 5], "d8": 6, "d9": 7, "sda": 8, "scl": 9}
    if channel_map != expected:
        raise FinalizeError("channel_map does not match bits 0 through 9")
    return expected


def validate_capture_command(value: object, raw_name: str, binary_name: str) -> list[str]:
    if not isinstance(value, list):
        raise FinalizeError("capture_command must be an argv array")
    command = [require_string(argument, "capture command argument") for argument in value]
    if len(command) != 15:
        raise FinalizeError("capture command does not match the canonical argv")
    expected_prefix = [
        "--driver",
        "dreamsourcelab-dslogic",
        "--channels",
        CHANNELS,
        "--config",
        "samplerate=2m:voltage_threshold=1.4-3.6:continuous=true",
        "--samples",
        str(REQUESTED_SAMPLES),
        "--output-format",
        "binary",
        "--output-file",
    ]
    if command[1:12] != expected_prefix or command[13:] != ["--loglevel", "4"]:
        raise FinalizeError("capture command does not match the canonical argv")
    if Path(command[0]).name != f".{binary_name}.partial":
        raise FinalizeError("capture command did not use the bound temporary executable")
    if Path(command[12]).name != f".{raw_name}.partial":
        raise FinalizeError("capture command output path is not the bound temporary raw file")
    return command


def validate_setup_command(value: object, binary_name: str) -> list[str]:
    if not isinstance(value, list):
        raise FinalizeError("setup_command must be an argv array")
    command = [require_string(argument, "setup command argument") for argument in value]
    if len(command) != 8 or command[1:] != [
        "--driver",
        "dreamsourcelab-dslogic",
        "--channels",
        CHANNELS,
        "--show",
        "--loglevel",
        "4",
    ]:
        raise FinalizeError("setup command does not match the canonical argv")
    if Path(command[0]).name != f".{binary_name}.partial":
        raise FinalizeError("setup command did not use the bound temporary executable")
    return command


def validate_recorder_command(
    value: object,
    *,
    recorder_name: str,
    template_name: str,
    rig_name: str,
    axis_name: str,
    manifest_name: str,
    session_name: str,
) -> list[str]:
    if not isinstance(value, list):
        raise FinalizeError("recorder_command must be an argv array")
    command = [require_string(argument, "recorder command argument") for argument in value]
    if len(command) != 12 or command[1] != "record-hand":
        raise FinalizeError("recorder command does not match the canonical argv")
    expected_names = (
        (command[0], f".{recorder_name}.partial"),
        (command[2], f".{template_name}.partial"),
        (command[3], f".{rig_name}.partial"),
        (command[4], f".{axis_name}.partial"),
        (command[6], f".{manifest_name}.partial"),
        (command[7], f".{session_name}.partial"),
    )
    if any(Path(argument).name != expected for argument, expected in expected_names):
        raise FinalizeError("recorder command does not bind the published partial artifacts")
    if not Path(command[5]).is_absolute():
        raise FinalizeError("recorder device path must be absolute")
    if command[8] != "--ready-fd" or command[10] != "--stop-fd":
        raise FinalizeError("recorder command is missing the private pipe flags")
    try:
        ready_fd = int(command[9], 10)
        stop_fd = int(command[11], 10)
    except ValueError as error:
        raise FinalizeError("recorder pipe descriptors must be decimal integers") from error
    if ready_fd < 3 or stop_fd < 3 or ready_fd == stop_fd:
        raise FinalizeError("recorder pipe descriptors must be distinct private descriptors")
    return command


def inspect_final_log(
    log_path: Path,
    *,
    expected_firmware_bundle_sha256: str,
    expected_stop_observed_raw_bytes: int,
    expected_session_bytes: int,
    expected_locked_manifest_sha256: str,
) -> tuple[int, str]:
    log_digest = hashlib.sha256()
    driver_receipt_count = 0
    first_driver_receipt: int | None = None
    firmware_lines = {entry["name"]: 0 for entry in FIRMWARE_INVENTORY}
    firmware_receipt_lines = {
        firmware["name"]: (
            "IKB_GATE0_FIRMWARE_V1 "
            f"name={firmware['name']} bytes={firmware['bytes']} "
            f"sha256={firmware['sha256']}\n"
        ).encode("ascii")
        for firmware in FIRMWARE_INVENTORY
    }
    final_line = b""

    with log_path.open("rb", buffering=0) as capture_log:
        while line := capture_log.readline(MAX_LOG_LINE_BYTES + 1):
            log_digest.update(line)
            if len(line) > MAX_LOG_LINE_BYTES:
                raise FinalizeError("capture log contains an overlong line")
            final_line = line
            if any(marker in line for marker in CAPTURE_FAILURE_MARKERS):
                raise FinalizeError(f"capture log contains failure marker: {line[:160]!r}")
            for match in DRIVER_RECEIPT.finditer(line):
                driver_receipt_count += 1
                if first_driver_receipt is None:
                    first_driver_receipt = int(match.group(1))
            for name, expected_line in firmware_receipt_lines.items():
                if line == expected_line:
                    firmware_lines[name] += 1

    if driver_receipt_count != 1 or first_driver_receipt is None:
        raise FinalizeError(
            f"expected one patched-driver receipt, found {driver_receipt_count}"
        )
    capture_match = CAPTURE_RECEIPT.fullmatch(final_line)
    if capture_match is None:
        raise FinalizeError("canonical capture receipt is not the final log line")
    completed_transfers = int(capture_match.group(1))
    stop_observed_raw_bytes = int(capture_match.group(2))
    session_bytes = int(capture_match.group(3))
    locked_manifest_sha256 = capture_match.group(4).decode("ascii")
    firmware_bundle_sha256 = capture_match.group(5).decode("ascii")
    if completed_transfers != first_driver_receipt:
        raise FinalizeError("wrapper and patched-driver transfer counts differ")
    if stop_observed_raw_bytes != expected_stop_observed_raw_bytes:
        raise FinalizeError("capture log and handoff stop observations differ")
    if session_bytes != expected_session_bytes:
        raise FinalizeError("capture log and handoff session sizes differ")
    if locked_manifest_sha256 != expected_locked_manifest_sha256:
        raise FinalizeError("capture log and handoff manifest hashes differ")
    if firmware_bundle_sha256 != expected_firmware_bundle_sha256:
        raise FinalizeError("capture receipt firmware bundle hash does not match")
    for name, count in firmware_lines.items():
        if count != 1:
            raise FinalizeError(f"expected one loaded-firmware receipt for {name}")
    return completed_transfers, log_digest.hexdigest()


def write_json_exclusive(path: Path, value: object) -> None:
    encoded = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")
    if len(encoded) > MAX_JSON_BYTES:
        raise FinalizeError(f"JSON artifact exceeds {MAX_JSON_BYTES} bytes")
    with path.open("xb", buffering=0) as output:
        output.write(encoded)
        output.flush()
        os.fsync(output.fileno())


def stop_child(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        process.wait()
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        process.wait()
        return
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()


def run_scorer(command: Sequence[str], *, executable_fd: int) -> None:
    # The scorer's stdout is informational only. Keep stderr on disk so a
    # noisy one-hour reduction cannot exhaust Crabbox memory.
    executable_path = f"/proc/self/fd/{executable_fd}"
    try:
        descriptor_metadata = os.fstat(executable_fd)
        proc_metadata = os.stat(executable_path)
    except OSError as error:
        raise FinalizeError(f"cannot resolve admitted scorer descriptor: {error}") from error
    if (descriptor_metadata.st_dev, descriptor_metadata.st_ino) != (
        proc_metadata.st_dev,
        proc_metadata.st_ino,
    ):
        raise FinalizeError("admitted scorer descriptor does not match its proc-fd path")

    with tempfile.TemporaryFile(mode="w+b") as stderr_log:
        process = subprocess.Popen(
            command,
            executable=executable_path,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=stderr_log,
            env={**os.environ, "LC_ALL": "C", "LANG": "C"},
            pass_fds=(executable_fd,),
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + SCORER_TIMEOUT_SECONDS
            status = process.poll()
            while status is None:
                if os.fstat(stderr_log.fileno()).st_size > MAX_SCORER_STDERR_BYTES:
                    raise FinalizeError(
                        "Gate 0A scorer stderr exceeded "
                        f"{MAX_SCORER_STDERR_BYTES} bytes"
                    )
                if time.monotonic() >= deadline:
                    raise FinalizeError(
                        f"Gate 0A scorer exceeded {SCORER_TIMEOUT_SECONDS} seconds"
                    )
                time.sleep(SCORER_POLL_INTERVAL_SECONDS)
                status = process.poll()
            if os.fstat(stderr_log.fileno()).st_size > MAX_SCORER_STDERR_BYTES:
                raise FinalizeError(
                    f"Gate 0A scorer stderr exceeded {MAX_SCORER_STDERR_BYTES} bytes"
                )
        except BaseException:
            stop_child(process)
            raise
        if process.returncode != 0:
            stderr_log.seek(0, os.SEEK_END)
            stderr_log.seek(max(0, stderr_log.tell() - 4096))
            detail = stderr_log.read().decode("utf-8", errors="replace")
            raise FinalizeError(
                f"Gate 0A scorer exited with status {process.returncode}: {detail}"
            )


def validate_report_hashes(
    report_path: Path,
    *,
    manifest_sha256: str,
    session_sha256: str,
    raw_sha256: str,
    sidecar_sha256: str,
) -> dict[str, object]:
    report = read_bounded_json(report_path, "Gate 0A report")
    if not isinstance(report, dict):
        raise FinalizeError("Gate 0A report must be a JSON object")
    if report.get("schema") != REPORT_SCHEMA:
        raise FinalizeError("Gate 0A report schema is not canonical")
    expected = {
        "source_manifest_sha256": manifest_sha256,
        "source_session_sha256": session_sha256,
        "raw_capture_sha256": raw_sha256,
        "capture_sidecar_sha256": sidecar_sha256,
    }
    for field, digest in expected.items():
        if require_sha256(report.get(field), f"report.{field}") != digest:
            raise FinalizeError(f"Gate 0A report {field} does not bind finalized evidence")

    criteria = report.get("criteria")
    trace = report.get("trace")
    if not isinstance(criteria, dict) or not isinstance(trace, dict):
        raise FinalizeError("Gate 0A report criteria and trace must be objects")
    deterministic_quiesce = criteria.get("deterministic_quiesce")
    if deterministic_quiesce not in {"pass", "fail", "inconclusive"}:
        raise FinalizeError("report.criteria.deterministic_quiesce is invalid")
    if trace.get("schema") != TRACE_SCHEMA:
        raise FinalizeError("Gate 0A trace evidence schema is not canonical")
    metrics = trace.get("metrics")
    required_metrics = {
        "final_sequence",
        "final_d8_rise_sample",
        "final_d8_fall_sample",
        "shutdown_unadmitted_int1_edges",
        "shutdown_transactions",
        "shutdown_scl_rising_edges",
        "terminal_marker_rise_sample",
        "terminal_marker_fall_sample",
        "terminal_marker_width_samples",
        "terminal_idle_samples",
        "host_response_time_ns",
        "deterministic_quiesce_violations",
    }
    if not isinstance(metrics, dict) or not required_metrics.issubset(metrics):
        raise FinalizeError("Gate 0A report lacks deterministic quiesce metrics")

    final_sequence = require_bounded_int(
        metrics["final_sequence"], U32_MAX, "report.trace.metrics.final_sequence"
    )
    final_d8_rise = require_optional_int(
        metrics["final_d8_rise_sample"],
        REQUESTED_SAMPLES,
        "report.trace.metrics.final_d8_rise_sample",
    )
    final_d8_fall = require_optional_int(
        metrics["final_d8_fall_sample"],
        REQUESTED_SAMPLES,
        "report.trace.metrics.final_d8_fall_sample",
    )
    shutdown_edges_value = metrics["shutdown_unadmitted_int1_edges"]
    if not isinstance(shutdown_edges_value, list) or len(shutdown_edges_value) != 6:
        raise FinalizeError("shutdown_unadmitted_int1_edges must contain six counters")
    for sensor_index, edge_count in enumerate(shutdown_edges_value):
        require_int(
            edge_count,
            None,
            f"report.trace.metrics.shutdown_unadmitted_int1_edges[{sensor_index}]",
        )
    shutdown_transactions = require_int(
        metrics["shutdown_transactions"],
        None,
        "report.trace.metrics.shutdown_transactions",
    )
    shutdown_scl_rising_edges = require_int(
        metrics["shutdown_scl_rising_edges"],
        None,
        "report.trace.metrics.shutdown_scl_rising_edges",
    )
    marker_rise = require_optional_int(
        metrics["terminal_marker_rise_sample"],
        REQUESTED_SAMPLES,
        "report.trace.metrics.terminal_marker_rise_sample",
    )
    marker_fall = require_optional_int(
        metrics["terminal_marker_fall_sample"],
        REQUESTED_SAMPLES,
        "report.trace.metrics.terminal_marker_fall_sample",
    )
    marker_width = require_optional_int(
        metrics["terminal_marker_width_samples"],
        U64_MAX,
        "report.trace.metrics.terminal_marker_width_samples",
    )
    terminal_idle_samples = require_int(
        metrics["terminal_idle_samples"],
        None,
        "report.trace.metrics.terminal_idle_samples",
    )
    if terminal_idle_samples > REQUESTED_SAMPLES:
        raise FinalizeError("terminal idle sample count exceeds the raw capture")
    host_response_time_ns = require_bounded_int(
        metrics["host_response_time_ns"],
        U64_MAX,
        "report.trace.metrics.host_response_time_ns",
    )
    quiesce_violations = require_int(
        metrics["deterministic_quiesce_violations"],
        None,
        "report.trace.metrics.deterministic_quiesce_violations",
    )

    if final_d8_rise is not None and final_d8_fall is not None and final_d8_fall <= final_d8_rise:
        raise FinalizeError("final D8 fall must follow its rise")
    if marker_rise is not None and marker_fall is not None and marker_fall <= marker_rise:
        raise FinalizeError("terminal marker fall must follow its rise")
    if (
        marker_rise is not None
        and marker_fall is not None
        and marker_width is not None
        and marker_fall - marker_rise != marker_width
    ):
        raise FinalizeError("terminal marker indices and width disagree")

    if deterministic_quiesce == "pass":
        if (
            final_d8_rise is None
            or final_d8_fall is None
            or marker_rise is None
            or marker_fall is None
            or marker_width is None
        ):
            raise FinalizeError("passing deterministic quiesce report has nullable evidence")
        if shutdown_transactions != 52 or shutdown_scl_rising_edges != 1422:
            raise FinalizeError("passing report does not contain the exact shutdown suffix")
        if not 200 <= marker_width <= 400:
            raise FinalizeError("passing report has an invalid terminal marker width")
        if marker_fall > MARKER_FALL_DEADLINE_SAMPLE:
            raise FinalizeError("passing report misses the terminal marker deadline")
        if marker_rise <= final_d8_fall:
            raise FinalizeError("passing report does not order final D8 before the marker")
        if terminal_idle_samples < 10_000_000:
            raise FinalizeError("passing report has less than five seconds of terminal idle")
        if host_response_time_ns > 750_000_000:
            raise FinalizeError("passing report exceeds the host receipt deadline")
        if quiesce_violations != 0:
            raise FinalizeError("passing report contains deterministic quiesce violations")
    return report


def publish_pair(
    sidecar_partial: Path,
    sidecar_final: Path,
    report_partial: Path,
    report_final: Path,
) -> None:
    created: list[Path] = []
    try:
        for partial, final in (
            (sidecar_partial, sidecar_final),
            (report_partial, report_final),
        ):
            os.link(partial, final, follow_symlinks=False)
            created.append(final)
        fsync_directory(sidecar_final.parent)
    except Exception:
        for final in reversed(created):
            try:
                final.unlink()
            except FileNotFoundError:
                pass
        fsync_directory(sidecar_final.parent)
        raise
    sidecar_partial.unlink()
    report_partial.unlink()
    fsync_directory(sidecar_final.parent)


def finalize(
    handoff_path: Path,
    sidecar_output: Path,
    report_output: Path,
    scorer: Path,
) -> tuple[dict[str, object], dict[str, object], str]:
    if os.environ.get("IKB_CRABBOX_FINALIZE") != "1":
        raise FinalizeError("refusing to finalize outside an explicit Crabbox")
    handoff = read_handoff(handoff_path)
    directory = handoff_path.parent.resolve()
    if sidecar_output.parent.resolve() != directory or report_output.parent.resolve() != directory:
        raise FinalizeError("sidecar and report must be written beside the capture artifacts")
    if sidecar_output == report_output:
        raise FinalizeError("sidecar and report outputs must be distinct")
    sidecar_partial = sidecar_output.with_name(f".{sidecar_output.name}.partial")
    report_partial = report_output.with_name(f".{report_output.name}.partial")
    for path in (sidecar_output, report_output, sidecar_partial, report_partial):
        if path.exists() or path.is_symlink():
            raise FinalizeError(f"refusing to overwrite finalization path: {path}")

    if handoff["schema"] != HANDOFF_SCHEMA:
        raise FinalizeError("unsupported handoff schema")
    require_int(handoff["word_bytes"], RAW_WORD_BYTES, "word_bytes")
    if handoff["byte_order"] != "little":
        raise FinalizeError("byte_order must be little")
    require_int(handoff["sample_rate_hz"], SAMPLE_RATE_HZ, "sample_rate_hz")
    channel_map = validate_channel_map(handoff["channel_map"])
    require_int(handoff["voltage_threshold_low_mv"], 1400, "voltage_threshold_low_mv")
    require_int(handoff["voltage_threshold_high_mv"], 3600, "voltage_threshold_high_mv")
    require_int(handoff["requested_samples"], REQUESTED_SAMPLES, "requested_samples")
    require_int(
        handoff["stop_window_first_byte"],
        STOP_WINDOW_FIRST_BYTE,
        "stop_window_first_byte",
    )
    require_int(
        handoff["stop_window_last_byte"],
        STOP_WINDOW_LAST_BYTE,
        "stop_window_last_byte",
    )
    stop_observed_raw_bytes = require_int(
        handoff["stop_observed_raw_bytes"], None, "stop_observed_raw_bytes"
    )
    if not STOP_WINDOW_FIRST_BYTE <= stop_observed_raw_bytes <= STOP_WINDOW_LAST_BYTE:
        raise FinalizeError("stop_observed_raw_bytes is outside the canonical stop window")
    maximum_raw_poll_interval_ns = require_int(
        handoff["maximum_raw_poll_interval_ns"], None, "maximum_raw_poll_interval_ns"
    )
    if maximum_raw_poll_interval_ns > MAX_RAW_POLL_INTERVAL_NS:
        raise FinalizeError("hardware handoff records an excessive raw polling interval")
    capture_started_unix_ms = require_int(
        handoff["capture_started_unix_ms"], None, "capture_started_unix_ms"
    )
    capture_finished_unix_ms = require_int(
        handoff["capture_finished_unix_ms"], None, "capture_finished_unix_ms"
    )
    if capture_finished_unix_ms <= capture_started_unix_ms:
        raise FinalizeError("capture_finished_unix_ms must be later than capture_started_unix_ms")
    if handoff["driver_base_commit"] != DRIVER_BASE_COMMIT:
        raise FinalizeError("driver base commit does not match the pinned stack")
    if handoff["sigrok_cli_base_commit"] != SIGROK_CLI_BASE_COMMIT:
        raise FinalizeError("sigrok-cli base commit does not match the pinned stack")
    if handoff["device_model"] != DEVICE_MODEL:
        raise FinalizeError("device model must be DSLogic Plus")
    require_int(handoff["capture_exit_status"], 0, "capture_exit_status")
    require_int(handoff["recorder_exit_status"], 0, "recorder_exit_status")

    raw_artifact = require_exact_keys(handoff["raw_capture"], {"path", "bytes"}, "raw_capture")
    raw_path = resolve_bound_path(directory, raw_artifact["path"], "raw_capture.path")
    require_int(raw_artifact["bytes"], EXPECTED_RAW_BYTES, "raw_capture.bytes")
    if raw_path.stat().st_size != EXPECTED_RAW_BYTES:
        raise FinalizeError("raw capture size changed after the hardware handoff")

    session_artifact = require_exact_keys(
        handoff["session_capture"], {"path", "bytes"}, "session_capture"
    )
    session_path = resolve_bound_path(
        directory, session_artifact["path"], "session_capture.path"
    )
    session_bytes = require_int(
        session_artifact["bytes"], session_path.stat().st_size, "session_capture.bytes"
    )
    require_int(verify_iks3_envelope(session_path), session_bytes, "session_capture.bytes")

    manifest_path, locked_manifest = validate_bound_artifact(
        handoff["locked_manifest"], directory, "locked_manifest", hash_required=True
    )
    _, capture_binary = validate_bound_artifact(
        handoff["capture_binary"], directory, "capture_binary", hash_required=True
    )
    _, recorder_binary = validate_bound_artifact(
        handoff["recorder_binary"], directory, "recorder_binary", hash_required=True
    )
    log_path, capture_log = validate_bound_artifact(
        handoff["capture_log"], directory, "capture_log", hash_required=False
    )
    _, recorder_log = validate_bound_artifact(
        handoff["recorder_log"], directory, "recorder_log", hash_required=True
    )
    _, firmware_bundle = validate_bound_artifact(
        handoff["device_firmware_bundle"],
        directory,
        "device_firmware_bundle",
        hash_required=True,
    )
    _, driver_patch = validate_bound_artifact(
        handoff["driver_patch"], directory, "driver_patch", hash_required=True
    )
    _, acquisition_template = validate_bound_artifact(
        handoff["acquisition_template"],
        directory,
        "acquisition_template",
        hash_required=True,
    )
    rig_path, rig_evidence = validate_bound_artifact(
        handoff["rig_evidence"], directory, "rig_evidence", hash_required=True
    )
    axis_path, axis_characterization = validate_bound_artifact(
        handoff["axis_characterization"],
        directory,
        "axis_characterization",
        hash_required=True,
    )
    if handoff["loaded_firmware"] != list(FIRMWARE_INVENTORY):
        raise FinalizeError("loaded_firmware does not match the fixed firmware bundle")

    validate_setup_command(handoff["setup_command"], capture_binary["path"])
    capture_command = validate_capture_command(
        handoff["capture_command"], raw_path.name, capture_binary["path"]
    )
    validate_recorder_command(
        handoff["recorder_command"],
        recorder_name=recorder_binary["path"],
        template_name=acquisition_template["path"],
        rig_name=rig_evidence["path"],
        axis_name=axis_characterization["path"],
        manifest_name=manifest_path.name,
        session_name=session_path.name,
    )
    _, capture_log_sha256 = inspect_final_log(
        log_path,
        expected_firmware_bundle_sha256=firmware_bundle["sha256"],
        expected_stop_observed_raw_bytes=stop_observed_raw_bytes,
        expected_session_bytes=session_bytes,
        expected_locked_manifest_sha256=locked_manifest["sha256"],
    )
    capture_log["sha256"] = capture_log_sha256

    scorer_image_fd = admit_scorer_image(
        scorer,
        expected_bytes=int(recorder_binary["bytes"]),
        expected_sha256=str(recorder_binary["sha256"]),
    )
    try:
        # These are the only full-file hashes authored by the finalizer. The raw
        # reducer later verifies the raw hash while deriving waveform evidence.
        session_sha256 = sha256_file(session_path)
        raw_sha256 = sha256_file(raw_path)
        sidecar = {
            "schema": SIDECAR_SCHEMA,
            "raw_capture_sha256": raw_sha256,
            "raw_capture_bytes": EXPECTED_RAW_BYTES,
            "word_bytes": RAW_WORD_BYTES,
            "byte_order": "little",
            "sample_rate_hz": SAMPLE_RATE_HZ,
            "channel_map": channel_map,
            "voltage_threshold_low_mv": 1400,
            "voltage_threshold_high_mv": 3600,
            "requested_samples": REQUESTED_SAMPLES,
            "capture_started_unix_ms": capture_started_unix_ms,
            "capture_finished_unix_ms": capture_finished_unix_ms,
            "capture_binary": capture_binary,
            "capture_log": capture_log,
            "device_firmware_bundle": firmware_bundle,
            "driver_base_commit": DRIVER_BASE_COMMIT,
            "sigrok_cli_base_commit": SIGROK_CLI_BASE_COMMIT,
            "device_model": DEVICE_MODEL,
            "driver_patch_sha256": driver_patch["sha256"],
            "capture_exit_status": 0,
            "command": capture_command,
        }
        write_json_exclusive(sidecar_partial, sidecar)
        sidecar_sha256 = sha256_file(sidecar_partial)

        scorer_command = [
            str(scorer),
            "score-gate0a",
            str(manifest_path),
            str(rig_path),
            str(axis_path),
            str(session_path),
            str(raw_path),
            str(sidecar_partial),
            str(report_partial),
        ]
        if any(
            len(argument.encode("utf-8")) > MAX_COMMAND_ARGUMENT_BYTES
            for argument in scorer_command
        ):
            raise FinalizeError("scorer command contains an overlong argument")
        run_scorer(scorer_command, executable_fd=scorer_image_fd)
        sync_regular_file(report_partial)
        report = validate_report_hashes(
            report_partial,
            manifest_sha256=locked_manifest["sha256"],
            session_sha256=session_sha256,
            raw_sha256=raw_sha256,
            sidecar_sha256=sidecar_sha256,
        )
        sync_regular_file(sidecar_partial)
        fsync_directory(directory)
        publish_pair(sidecar_partial, sidecar_output, report_partial, report_output)
        return sidecar, report, session_sha256
    finally:
        os.close(scorer_image_fd)


def parse_arguments(arguments: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--handoff", required=True, type=Path)
    parser.add_argument("--sidecar-output", required=True, type=Path)
    parser.add_argument("--report-output", required=True, type=Path)
    parser.add_argument("--scorer", required=True, type=Path)
    return parser.parse_args(arguments)


def main(arguments: Sequence[str] | None = None) -> int:
    options = parse_arguments(sys.argv[1:] if arguments is None else arguments)
    previous_handlers: dict[int, signal.Handlers] = {}

    def abort_from_signal(signum: int, _frame: object) -> None:
        for abort_signal in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
            signal.signal(abort_signal, signal.SIG_IGN)
        raise FinalizeError(f"finalization aborted by {signal.Signals(signum).name}")

    for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        previous_handlers[signum] = signal.signal(signum, abort_from_signal)
    try:
        sidecar, _, session_sha256 = finalize(
            options.handoff,
            options.sidecar_output,
            options.report_output,
            options.scorer,
        )
    except (FinalizeError, OSError, subprocess.SubprocessError) as error:
        print(f"gate0a-finalize: {error}", file=sys.stderr)
        return 1
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
    print(f"raw SHA-256: {sidecar['raw_capture_sha256']}")
    print(f"IKS3 SHA-256: {session_sha256}")
    print(f"sidecar: {options.sidecar_output}")
    print(f"report: {options.report_output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
