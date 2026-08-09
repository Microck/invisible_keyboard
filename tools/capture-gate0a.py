#!/usr/bin/env python3
"""Supervise the canonical Gate 0A analyzer and duplex controller recorder.

The hardware host stages and syncs evidence but does not decode the waveform or
hash the 14.44 GB raw file. Decoding and large-file hashing belong to the
Crabbox finalizer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
from dataclasses import dataclass, fields
from pathlib import Path
from typing import BinaryIO, Final, Iterable, Sequence


SAMPLE_RATE_HZ: Final = 2_000_000
REQUESTED_SAMPLES: Final = 7_220_000_000
RAW_WORD_BYTES: Final = 2
EXPECTED_RAW_BYTES: Final = REQUESTED_SAMPLES * RAW_WORD_BYTES
STOP_WINDOW_FIRST_BYTE: Final = 14_416_000_000
STOP_WINDOW_LAST_BYTE: Final = 14_417_000_000
READY_WINDOW_LAST_BYTE: Final = 12_000_000
MINIMUM_FREE_MARGIN_BYTES: Final = 512 * 1024 * 1024
CHANNELS: Final = "0,1,2,3,4,5,6,7,8,9"
DRIVER_BASE_COMMIT: Final = "0bc2487778e660f4d3116729b6f4aee2b1996bb0"
SIGROK_CLI_BASE_COMMIT: Final = "f44dd91347e7ac797cefc23162b9fcf0b7329f1f"
DEVICE_MODEL: Final = "DSLogic Plus"
HANDOFF_SCHEMA: Final = "invisible-keyboard-gate0-dslogic-handoff-v2"
FIRMWARE_SCHEMA: Final = "invisible-keyboard-dslogic-plus-firmware-v1"
DSVIEW_COMMIT: Final = "886b847c21c606df3138ce7ad8f8e8c363ee758b"
SIGROK_EXTRACTOR_COMMIT: Final = "69747ca48b3d17ac7532439e487b9ba829828cfd"
MAX_LOG_LINE_BYTES: Final = 4096
MAX_CAPTURE_CHILD_LOG_BYTES: Final = 64 * 1024 * 1024
MAX_RECORDER_LOG_BYTES: Final = 8 * 1024 * 1024
MAX_HANDOFF_BYTES: Final = 1024 * 1024
MAX_COMMAND_ARGUMENT_BYTES: Final = 4096
COPY_BUFFER_BYTES: Final = 1024 * 1024
SETUP_TIMEOUT_SECONDS: Final = 60
RECORDER_READY_TIMEOUT_SECONDS: Final = 60
RECORDER_FINALIZE_TIMEOUT_SECONDS: Final = 15
CAPTURE_TIMEOUT_SECONDS: Final = REQUESTED_SAMPLES // SAMPLE_RATE_HZ + 300
RAW_POLL_INTERVAL_NS: Final = 1_000_000
MAX_RAW_POLL_INTERVAL_NS: Final = 10_000_000
LOG_SIZE_POLL_INTERVAL_NS: Final = 10_000_000
READY_BYTE: Final = b"\x01"
STOP_BYTE: Final = b"\x01"

DRIVER_RECEIPT = re.compile(
    rb"IKB_DSLOGIC_TRANSFER_V1 completed_transfers=([1-9][0-9]*) anomalies=0"
)
CAPTURE_FAILURE_MARKERS: Final = (
    b"IKB_DSLOGIC_TRANSFER_FAILURE_V1",
    b"Capture rejected",
    b"CRITICAL",
    b"WARNING",
)
SAFE_STEM = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")


class CaptureError(RuntimeError):
    """A fail-closed capture, process, pipe, or artifact error."""


@dataclass(frozen=True)
class FirmwareFile:
    installed_name: str
    source_name: str
    bytes: int
    sha256: str


FIRMWARE_FILES: Final = (
    FirmwareFile(
        installed_name="dreamsourcelab-dslogic-plus-fpga.fw",
        source_name="DSLogicPlus.bin",
        bytes=341_160,
        sha256="e16c70e4af9181044e3ec3a363e4bb109f1d731b4f9f046f8bc1d1015a7842a7",
    ),
    FirmwareFile(
        installed_name="dreamsourcelab-dslogic-plus-fx2.fw",
        source_name="DSLogicPlus.fw",
        bytes=8_120,
        sha256="9197e08dd33a9cda61cc8cd3de559cbae9b049b2dfa827c089ffe0528f5dcc5e",
    ),
)
DSVIEW_COPYING_BYTES: Final = 35_821
DSVIEW_COPYING_SHA256: Final = (
    "0b383d5a63da644f628d99c33976ea6487ed89aaa59f0b3257992deac1171e6b"
)
EXPECTED_FIRMWARE_MEMBERS: Final = (
    "COPYING.dsview",
    "firmware-manifest.json",
    FIRMWARE_FILES[0].installed_name,
    FIRMWARE_FILES[1].installed_name,
)


@dataclass(frozen=True)
class OutputPaths:
    raw: Path
    session: Path
    locked_manifest: Path
    capture_log: Path
    recorder_log: Path
    capture_binary: Path
    recorder_binary: Path
    driver_patch: Path
    firmware_bundle: Path
    template: Path
    rig_evidence: Path
    axis_characterization: Path
    handoff: Path
    partial_raw: Path
    partial_session: Path
    partial_locked_manifest: Path
    partial_capture_log: Path
    partial_recorder_log: Path
    partial_capture_binary: Path
    partial_recorder_binary: Path
    partial_driver_patch: Path
    partial_firmware_bundle: Path
    partial_template: Path
    partial_rig_evidence: Path
    partial_axis_characterization: Path
    partial_handoff: Path

    def all_paths(self) -> tuple[Path, ...]:
        return tuple(getattr(self, field.name) for field in fields(self))

    def publication_pairs(self) -> tuple[tuple[Path, Path], ...]:
        # The handoff is the final commit marker for the complete artifact set.
        return (
            (self.partial_capture_binary, self.capture_binary),
            (self.partial_recorder_binary, self.recorder_binary),
            (self.partial_driver_patch, self.driver_patch),
            (self.partial_firmware_bundle, self.firmware_bundle),
            (self.partial_template, self.template),
            (self.partial_rig_evidence, self.rig_evidence),
            (self.partial_axis_characterization, self.axis_characterization),
            (self.partial_raw, self.raw),
            (self.partial_session, self.session),
            (self.partial_locked_manifest, self.locked_manifest),
            (self.partial_capture_log, self.capture_log),
            (self.partial_recorder_log, self.recorder_log),
            (self.partial_handoff, self.handoff),
        )


def output_paths(output_directory: Path, stem: str) -> OutputPaths:
    if not SAFE_STEM.fullmatch(stem) or stem in {".", ".."}:
        raise CaptureError("capture name must be a safe 1-128 byte filename stem")

    def final(suffix: str) -> Path:
        return output_directory / f"{stem}{suffix}"

    def partial(suffix: str) -> Path:
        return output_directory / f".{stem}{suffix}.partial"

    return OutputPaths(
        raw=final(".gate0a.raw"),
        session=final(".iks3"),
        locked_manifest=final(".locked-manifest.json"),
        capture_log=final(".capture.log"),
        recorder_log=final(".recorder.log"),
        capture_binary=final(".sigrok-cli"),
        recorder_binary=final(".invisible-keyboard"),
        driver_patch=final(".libsigrok.patch"),
        firmware_bundle=final(".device-firmware.tar"),
        template=final(".acquisition-template.json"),
        rig_evidence=final(".rig-evidence.json"),
        axis_characterization=final(".axis-characterization.json"),
        handoff=final(".handoff.json"),
        partial_raw=partial(".gate0a.raw"),
        partial_session=partial(".iks3"),
        partial_locked_manifest=partial(".locked-manifest.json"),
        partial_capture_log=partial(".capture.log"),
        partial_recorder_log=partial(".recorder.log"),
        partial_capture_binary=partial(".sigrok-cli"),
        partial_recorder_binary=partial(".invisible-keyboard"),
        partial_driver_patch=partial(".libsigrok.patch"),
        partial_firmware_bundle=partial(".device-firmware.tar"),
        partial_template=partial(".acquisition-template.json"),
        partial_rig_evidence=partial(".rig-evidence.json"),
        partial_axis_characterization=partial(".axis-characterization.json"),
        partial_handoff=partial(".handoff.json"),
    )


def require_regular_file(path: Path, *, executable: bool = False) -> os.stat_result:
    try:
        metadata = path.lstat()
    except OSError as error:
        raise CaptureError(f"cannot inspect {path}: {error}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise CaptureError(f"expected a regular, non-symlink file: {path}")
    if executable and metadata.st_mode & 0o111 == 0:
        raise CaptureError(f"executable input has no execute bit: {path}")
    return metadata


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb", buffering=0) as source:
        while chunk := source.read(8 * 1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def copy_exclusive(source: Path, destination: Path, mode: int) -> None:
    require_regular_file(source)
    with source.open("rb", buffering=0) as reader, destination.open(
        "xb", buffering=0
    ) as writer:
        shutil.copyfileobj(reader, writer, COPY_BUFFER_BYTES)
        writer.flush()
        os.fsync(writer.fileno())
    destination.chmod(mode)


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
    """Fail early on an unfinalized session without duplicating the Rust audit."""

    metadata = require_regular_file(path)
    if metadata.st_size < 80:
        raise CaptureError("finalized IKS3 session is too short")
    with path.open("rb", buffering=0) as session:
        header = session.read(8)
        session.seek(-16, os.SEEK_END)
        footer = session.read(4)
    if header != b"IKS3\x03\x00\x00\x00" or footer != b"END3":
        raise CaptureError("recorder output lacks the canonical IKS3 header or END3 footer")
    return metadata.st_size


def publish_artifact_set(paths: OutputPaths) -> None:
    """Link the complete set before removing partial names.

    Linking first makes publication rollback safe: every partial remains until
    every final name, including the handoff commit marker, exists.
    """

    created: list[Path] = []
    pairs = paths.publication_pairs()
    try:
        for partial, final in pairs:
            os.link(partial, final, follow_symlinks=False)
            created.append(final)
        fsync_directory(paths.raw.parent)
    except Exception:
        for final in reversed(created):
            try:
                final.unlink()
            except FileNotFoundError:
                pass
        fsync_directory(paths.raw.parent)
        raise

    for partial, _ in pairs:
        partial.unlink()
    fsync_directory(paths.raw.parent)


def hash_stream(source: BinaryIO) -> tuple[int, str, bytes]:
    payload = source.read()
    return len(payload), hashlib.sha256(payload).hexdigest(), payload


def expected_firmware_manifest() -> dict[str, object]:
    return {
        "schema": FIRMWARE_SCHEMA,
        "dsview_commit": DSVIEW_COMMIT,
        "sigrok_extractor_commit": SIGROK_EXTRACTOR_COMMIT,
        "files": [
            {
                "installed_name": firmware.installed_name,
                "source_name": firmware.source_name,
                "bytes": firmware.bytes,
                "sha256": firmware.sha256,
            }
            for firmware in FIRMWARE_FILES
        ],
    }


def stage_firmware_bundle(bundle: Path, stage_directory: Path) -> list[dict[str, object]]:
    """Validate the canonical archive and extract only its two firmware files."""

    bundle_metadata = require_regular_file(bundle)
    if bundle_metadata.st_size > 2 * 1024 * 1024:
        raise CaptureError("firmware tar exceeds the two MiB bound")
    try:
        archive = tarfile.open(bundle, mode="r:")
    except (OSError, tarfile.TarError) as error:
        raise CaptureError(f"invalid firmware tar: {error}") from error

    with archive:
        members = archive.getmembers()
        if tuple(member.name for member in members) != EXPECTED_FIRMWARE_MEMBERS:
            raise CaptureError("firmware tar member list or order is not canonical")
        for member in members:
            if not member.isfile() or member.uid != 0 or member.gid != 0:
                raise CaptureError(f"non-canonical firmware member: {member.name}")
            if member.mtime != 0 or stat.S_IMODE(member.mode) != 0o644:
                raise CaptureError(f"non-canonical firmware metadata: {member.name}")
        expected_sizes = {
            "COPYING.dsview": DSVIEW_COPYING_BYTES,
            "firmware-manifest.json": 64 * 1024,
            **{firmware.installed_name: firmware.bytes for firmware in FIRMWARE_FILES},
        }
        for member in members:
            expected_size = expected_sizes[member.name]
            if member.name == "firmware-manifest.json":
                if member.size > expected_size:
                    raise CaptureError("firmware manifest exceeds 64 KiB")
            elif member.size != expected_size:
                raise CaptureError(f"firmware member size mismatch: {member.name}")

        extracted: dict[str, tuple[int, str, bytes]] = {}
        for member in members:
            source = archive.extractfile(member)
            if source is None:
                raise CaptureError(f"cannot read firmware member: {member.name}")
            with source:
                extracted[member.name] = hash_stream(source)

    copying_bytes, copying_sha256, _ = extracted["COPYING.dsview"]
    if copying_bytes != DSVIEW_COPYING_BYTES or copying_sha256 != DSVIEW_COPYING_SHA256:
        raise CaptureError("DSView license file identity does not match the pinned source")

    _, _, manifest_payload = extracted["firmware-manifest.json"]
    try:
        manifest = json.loads(manifest_payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise CaptureError(f"invalid firmware manifest: {error}") from error
    if manifest != expected_firmware_manifest():
        raise CaptureError("firmware manifest does not match the pinned contract")

    inventory: list[dict[str, object]] = []
    for firmware in FIRMWARE_FILES:
        byte_count, sha256, payload = extracted[firmware.installed_name]
        if byte_count != firmware.bytes or sha256 != firmware.sha256:
            raise CaptureError(f"firmware identity mismatch: {firmware.installed_name}")
        destination = stage_directory / firmware.installed_name
        with destination.open("xb", buffering=0) as writer:
            writer.write(payload)
            writer.flush()
            os.fsync(writer.fileno())
        destination.chmod(0o644)
        inventory.append(
            {"name": firmware.installed_name, "bytes": byte_count, "sha256": sha256}
        )
    fsync_directory(stage_directory)
    return inventory


def build_capture_command(capture_binary: Path, raw_output: Path) -> list[str]:
    command = [
        str(capture_binary),
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
        str(raw_output),
        "--loglevel",
        "4",
    ]
    validate_command_arguments(command)
    return command


def build_setup_command(capture_binary: Path) -> list[str]:
    command = [
        str(capture_binary),
        "--driver",
        "dreamsourcelab-dslogic",
        "--channels",
        CHANNELS,
        "--show",
        "--loglevel",
        "4",
    ]
    validate_command_arguments(command)
    return command


def build_recorder_command(
    recorder_binary: Path,
    template: Path,
    rig_evidence: Path,
    axis_characterization: Path,
    device: str,
    locked_manifest: Path,
    session: Path,
    ready_fd: int,
    stop_fd: int,
) -> list[str]:
    command = [
        str(recorder_binary),
        "record-hand",
        str(template),
        str(rig_evidence),
        str(axis_characterization),
        device,
        str(locked_manifest),
        str(session),
        "--ready-fd",
        str(ready_fd),
        "--stop-fd",
        str(stop_fd),
    ]
    validate_command_arguments(command)
    return command


def validate_command_arguments(command: Sequence[str]) -> None:
    if any(len(argument.encode("utf-8")) > MAX_COMMAND_ARGUMENT_BYTES for argument in command):
        raise CaptureError("child command contains an overlong argument")


def inspect_capture_log(
    log_path: Path,
    stage_directory: Path,
    *,
    expected_firmware_opens: dict[str, int],
    require_driver_receipt: bool,
) -> int | None:
    receipt_count = 0
    first_receipt: int | None = None
    opened_firmware = {firmware.installed_name: 0 for firmware in FIRMWARE_FILES}
    opened_lines = {
        firmware.installed_name: (
            f"Opened '{stage_directory / firmware.installed_name}'."
        ).encode("utf-8")
        for firmware in FIRMWARE_FILES
    }

    with log_path.open("rb") as capture_log:
        while line := capture_log.readline(MAX_LOG_LINE_BYTES + 1):
            if len(line) > MAX_LOG_LINE_BYTES:
                raise CaptureError("capture log contains an overlong line")
            if any(marker in line for marker in CAPTURE_FAILURE_MARKERS):
                raise CaptureError(f"capture log contains failure marker: {line[:160]!r}")
            for match in DRIVER_RECEIPT.finditer(line):
                receipt_count += 1
                if first_receipt is None:
                    first_receipt = int(match.group(1))
            for name, opened_line in opened_lines.items():
                if opened_line in line:
                    opened_firmware[name] += 1

    expected_receipts = 1 if require_driver_receipt else 0
    if receipt_count != expected_receipts:
        raise CaptureError(
            f"expected {expected_receipts} patched-driver receipt(s), found {receipt_count}"
        )
    for name, count in opened_firmware.items():
        expected_count = expected_firmware_opens[name]
        if count != expected_count:
            raise CaptureError(
                f"expected the staged firmware {name} to be opened {expected_count} "
                f"time(s), found {count}; power-cycle the analyzer before retrying"
            )
    if first_receipt is None:
        return None
    if first_receipt > 0xFFFF_FFFF_FFFF_FFFF:
        raise CaptureError("completed transfer count exceeds uint64")
    return first_receipt


def raw_size(raw_path: Path) -> int:
    try:
        metadata = raw_path.lstat()
    except FileNotFoundError:
        return 0
    except OSError as error:
        raise CaptureError(f"cannot inspect partial raw file: {error}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise CaptureError("partial raw output is not a regular non-symlink file")
    return metadata.st_size


def enforce_log_limit(descriptor: int, limit: int, label: str) -> None:
    try:
        size = os.fstat(descriptor).st_size
    except OSError as error:
        raise CaptureError(f"cannot inspect {label}: {error}") from error
    if size > limit:
        raise CaptureError(f"{label} exceeded its {limit}-byte limit")


def consume_readiness(fd: int, payload: bytearray) -> bool:
    """Read a nonblocking readiness pipe and require one byte followed by EOF."""

    while True:
        try:
            chunk = os.read(fd, 2)
        except BlockingIOError:
            return False
        if not chunk:
            if bytes(payload) != READY_BYTE:
                raise CaptureError("recorder readiness must be exactly one 0x01 byte")
            return True
        payload.extend(chunk)
        if len(payload) > 1 or bytes(payload) != READY_BYTE:
            raise CaptureError("recorder readiness must be exactly one 0x01 byte")


@dataclass(frozen=True)
class SupervisionReceipt:
    stop_observed_raw_bytes: int
    stop_written_monotonic_ns: int
    maximum_raw_poll_interval_ns: int
    capture_exit_status: int
    recorder_exit_status: int
    completed_transfers: int


def supervise_children(
    *,
    capture_process: subprocess.Popen[bytes],
    recorder_process: subprocess.Popen[bytes],
    raw_path: Path,
    ready_read_fd: int,
    stop_write_fd: int,
    capture_log_fd: int,
    capture_log_path: Path,
    stage_directory: Path,
    recorder_log_fd: int,
    ready_timeout_seconds: float = RECORDER_READY_TIMEOUT_SECONDS,
    capture_timeout_seconds: float = CAPTURE_TIMEOUT_SECONDS,
    recorder_finalize_timeout_seconds: float = RECORDER_FINALIZE_TIMEOUT_SECONDS,
) -> SupervisionReceipt:
    """Supervise both processes and issue the only valid local stop event."""

    os.set_blocking(ready_read_fd, False)
    started_ns = time.monotonic_ns()
    ready_deadline_ns = started_ns + int(ready_timeout_seconds * 1_000_000_000)
    capture_deadline_ns = started_ns + int(capture_timeout_seconds * 1_000_000_000)
    readiness = bytearray()
    ready = False
    stop_observed_raw_bytes: int | None = None
    stop_written_ns: int | None = None
    previous_size = 0
    previous_poll_ns: int | None = None
    maximum_poll_interval_ns = 0
    next_log_poll_ns = started_ns

    while True:
        poll_started_ns = time.monotonic_ns()
        if previous_poll_ns is not None:
            poll_interval_ns = poll_started_ns - previous_poll_ns
            maximum_poll_interval_ns = max(maximum_poll_interval_ns, poll_interval_ns)
            if poll_interval_ns > MAX_RAW_POLL_INTERVAL_NS:
                raise CaptureError(
                    f"raw-size polling gap was {poll_interval_ns} ns; maximum is "
                    f"{MAX_RAW_POLL_INTERVAL_NS} ns"
                )
        previous_poll_ns = poll_started_ns

        if poll_started_ns >= next_log_poll_ns:
            enforce_log_limit(
                capture_log_fd, MAX_CAPTURE_CHILD_LOG_BYTES, "sigrok-cli log"
            )
            enforce_log_limit(
                recorder_log_fd, MAX_RECORDER_LOG_BYTES, "recorder log"
            )
            next_log_poll_ns = poll_started_ns + LOG_SIZE_POLL_INTERVAL_NS

        current_size = raw_size(raw_path)
        if current_size < previous_size:
            raise CaptureError("partial raw file shrank during capture")
        if current_size > EXPECTED_RAW_BYTES:
            raise CaptureError("partial raw file exceeded the exact capture size")
        previous_size = current_size

        if not ready:
            ready = consume_readiness(ready_read_fd, readiness)
            if current_size > READY_WINDOW_LAST_BYTE:
                raise CaptureError(
                    f"recorder readiness exceeded the {READY_WINDOW_LAST_BYTE}-byte boot window"
                )
            if not ready and poll_started_ns >= ready_deadline_ns:
                raise CaptureError("recorder did not publish readiness before its deadline")

        if stop_written_ns is None and current_size >= STOP_WINDOW_FIRST_BYTE:
            if current_size > STOP_WINDOW_LAST_BYTE:
                raise CaptureError(
                    f"first eligible raw-size observation was {current_size}; stop window ends "
                    f"at {STOP_WINDOW_LAST_BYTE}"
                )
            if not ready:
                raise CaptureError("raw stop window arrived before recorder readiness")
            try:
                written = os.write(stop_write_fd, STOP_BYTE)
            except BrokenPipeError as error:
                raise CaptureError("recorder stop pipe closed before the stop request") from error
            if written != 1:
                raise CaptureError("recorder stop pipe accepted a short write")
            stop_observed_raw_bytes = current_size
            stop_written_ns = time.monotonic_ns()

        recorder_status = recorder_process.poll()
        if recorder_status is not None:
            raise CaptureError(
                f"recorder exited with status {recorder_status} before analyzer-success EOF"
            )

        capture_status = capture_process.poll()
        if capture_status is not None:
            if capture_status != 0:
                raise CaptureError(f"sigrok-cli exited with status {capture_status}")
            if stop_written_ns is None or stop_observed_raw_bytes is None:
                raise CaptureError("sigrok-cli completed before the timed stop request")
            final_size = raw_size(raw_path)
            if final_size != EXPECTED_RAW_BYTES:
                raise CaptureError(
                    f"raw capture is {final_size} bytes; expected {EXPECTED_RAW_BYTES}"
                )
            sync_regular_file(raw_path)
            enforce_log_limit(
                capture_log_fd, MAX_CAPTURE_CHILD_LOG_BYTES, "sigrok-cli log"
            )
            completed_transfers = inspect_capture_log(
                capture_log_path,
                stage_directory,
                expected_firmware_opens={
                    FIRMWARE_FILES[0].installed_name: 2,
                    FIRMWARE_FILES[1].installed_name: 1,
                },
                require_driver_receipt=True,
            )
            if completed_transfers is None:
                raise CaptureError("patched-driver receipt is missing")

            # EOF on this pipe is a success capability. It is issued only
            # after the complete analyzer output has passed admission.
            os.close(stop_write_fd)
            stop_write_fd = -1
            recorder_deadline_ns = time.monotonic_ns() + int(
                recorder_finalize_timeout_seconds * 1_000_000_000
            )
            recorder_status = recorder_process.poll()
            while recorder_status is None:
                enforce_log_limit(
                    recorder_log_fd, MAX_RECORDER_LOG_BYTES, "recorder log"
                )
                if time.monotonic_ns() >= recorder_deadline_ns:
                    raise CaptureError(
                        "recorder did not finish after analyzer-success EOF"
                    )
                time.sleep(LOG_SIZE_POLL_INTERVAL_NS / 1_000_000_000)
                recorder_status = recorder_process.poll()
            enforce_log_limit(
                recorder_log_fd, MAX_RECORDER_LOG_BYTES, "recorder log"
            )
            if recorder_status != 0:
                raise CaptureError(f"recorder exited with status {recorder_status}")
            return SupervisionReceipt(
                stop_observed_raw_bytes=stop_observed_raw_bytes,
                stop_written_monotonic_ns=stop_written_ns,
                maximum_raw_poll_interval_ns=maximum_poll_interval_ns,
                capture_exit_status=capture_status,
                recorder_exit_status=recorder_status,
                completed_transfers=completed_transfers,
            )

        if poll_started_ns >= capture_deadline_ns:
            raise CaptureError(
                f"capture children exceeded {capture_timeout_seconds:g} seconds"
            )
        sleep_ns = RAW_POLL_INTERVAL_NS - (time.monotonic_ns() - poll_started_ns)
        if sleep_ns > 0:
            time.sleep(sleep_ns / 1_000_000_000)


def append_capture_receipts(
    log_path: Path,
    firmware_inventory: Iterable[dict[str, object]],
    firmware_bundle_sha256: str,
    supervision: SupervisionReceipt,
    session_bytes: int,
    locked_manifest_sha256: str,
) -> None:
    with log_path.open("r+b", buffering=0) as capture_log:
        capture_log.seek(0, os.SEEK_END)
        if capture_log.tell() > 0:
            capture_log.seek(-1, os.SEEK_END)
            if capture_log.read(1) != b"\n":
                capture_log.write(b"\n")
            else:
                capture_log.seek(0, os.SEEK_END)
        for firmware in firmware_inventory:
            capture_log.write(
                (
                    "IKB_GATE0_FIRMWARE_V1 "
                    f"name={firmware['name']} bytes={firmware['bytes']} "
                    f"sha256={firmware['sha256']}\n"
                ).encode("ascii")
            )
        capture_log.write(
            (
                f"IKB_GATE0_CAPTURE_V2 samples={REQUESTED_SAMPLES} "
                f"completed_transfers={supervision.completed_transfers} anomalies=0 "
                f"stop_observed_raw_bytes={supervision.stop_observed_raw_bytes} "
                f"session_bytes={session_bytes} "
                f"locked_manifest_sha256={locked_manifest_sha256} "
                f"firmware_bundle_sha256={firmware_bundle_sha256}\n"
            ).encode("ascii")
        )
        capture_log.flush()
        os.fsync(capture_log.fileno())


def bound_artifact(
    path: Path, *, include_hash: bool = True, published_name: str | None = None
) -> dict[str, object]:
    metadata = require_regular_file(path)
    artifact: dict[str, object] = {
        "path": published_name or path.name,
        "bytes": metadata.st_size,
    }
    if include_hash:
        artifact["sha256"] = sha256_file(path)
    return artifact


def write_json_exclusive(path: Path, value: object) -> None:
    encoded = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")
    if len(encoded) > MAX_HANDOFF_BYTES:
        raise CaptureError("handoff metadata exceeds one MiB")
    with path.open("xb", buffering=0) as output:
        output.write(encoded)
        output.flush()
        os.fsync(output.fileno())


def stop_child(process: subprocess.Popen[bytes] | None) -> None:
    if process is None:
        return
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


def run_child(
    command: Sequence[str],
    *,
    capture_log: BinaryIO,
    environment: dict[str, str],
    timeout_seconds: float,
) -> int:
    process = subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=capture_log,
        env=environment,
        start_new_session=True,
    )
    try:
        deadline_ns = time.monotonic_ns() + int(timeout_seconds * 1_000_000_000)
        status = process.poll()
        while status is None:
            enforce_log_limit(
                capture_log.fileno(), MAX_CAPTURE_CHILD_LOG_BYTES, "sigrok-cli log"
            )
            if time.monotonic_ns() >= deadline_ns:
                raise CaptureError(f"child process exceeded {timeout_seconds:g} seconds")
            time.sleep(LOG_SIZE_POLL_INTERVAL_NS / 1_000_000_000)
            status = process.poll()
        enforce_log_limit(
            capture_log.fileno(), MAX_CAPTURE_CHILD_LOG_BYTES, "sigrok-cli log"
        )
        return status
    except KeyboardInterrupt:
        stop_child(process)
        raise CaptureError("capture interrupted") from None
    except BaseException:
        stop_child(process)
        raise


def run_capture(
    *,
    sigrok_cli: Path,
    recorder: Path,
    driver_patch: Path,
    firmware_bundle: Path,
    template: Path,
    rig_evidence: Path,
    axis_characterization: Path,
    device: str,
    output_directory: Path,
    stem: str,
) -> OutputPaths:
    if platform.system() != "Linux" or sys.byteorder != "little":
        raise CaptureError("the canonical capture host must be little-endian Linux")
    if sys.maxsize <= 0xFFFF_FFFF:
        raise CaptureError("the canonical capture host must be a 64-bit process")
    if not device or len(device.encode("utf-8")) > MAX_COMMAND_ARGUMENT_BYTES:
        raise CaptureError("controller device must be a bounded non-empty path")

    sigrok_metadata = require_regular_file(sigrok_cli, executable=True)
    recorder_metadata = require_regular_file(recorder, executable=True)
    for source in (
        driver_patch,
        firmware_bundle,
        template,
        rig_evidence,
        axis_characterization,
    ):
        require_regular_file(source)
    if not output_directory.is_dir() or output_directory.is_symlink():
        raise CaptureError("output directory must be an existing non-symlink directory")

    paths = output_paths(output_directory, stem)
    collisions = [path for path in paths.all_paths() if path.exists() or path.is_symlink()]
    if collisions:
        raise CaptureError(f"refusing to overwrite existing path: {collisions[0]}")

    free_bytes = shutil.disk_usage(output_directory).free
    required_bytes = EXPECTED_RAW_BYTES + MINIMUM_FREE_MARGIN_BYTES
    if free_bytes < required_bytes:
        raise CaptureError(
            f"capture filesystem has {free_bytes} free bytes; at least {required_bytes} required"
        )

    for source, destination, mode in (
        (sigrok_cli, paths.partial_capture_binary, 0o755),
        (recorder, paths.partial_recorder_binary, 0o755),
        (driver_patch, paths.partial_driver_patch, 0o644),
        (firmware_bundle, paths.partial_firmware_bundle, 0o644),
        (template, paths.partial_template, 0o644),
        (rig_evidence, paths.partial_rig_evidence, 0o644),
        (axis_characterization, paths.partial_axis_characterization, 0o644),
    ):
        copy_exclusive(source, destination, mode)
    if paths.partial_capture_binary.stat().st_size != sigrok_metadata.st_size:
        raise CaptureError("capture executable copy is short")
    if paths.partial_recorder_binary.stat().st_size != recorder_metadata.st_size:
        raise CaptureError("recorder executable copy is short")

    firmware_bundle_artifact = bound_artifact(
        paths.partial_firmware_bundle, published_name=paths.firmware_bundle.name
    )
    firmware_bundle_sha256 = firmware_bundle_artifact["sha256"]
    if not isinstance(firmware_bundle_sha256, str):
        raise CaptureError("firmware bundle SHA-256 was not computed")

    capture_process: subprocess.Popen[bytes] | None = None
    recorder_process: subprocess.Popen[bytes] | None = None
    ready_read_fd = -1
    ready_write_fd = -1
    stop_read_fd = -1
    stop_write_fd = -1
    with tempfile.TemporaryDirectory(
        prefix=f".{stem}.firmware-stage.", dir=output_directory
    ) as stage_name:
        stage_directory = Path(stage_name)
        firmware_inventory = stage_firmware_bundle(
            paths.partial_firmware_bundle, stage_directory
        )
        setup_command = build_setup_command(paths.partial_capture_binary)
        capture_command = build_capture_command(
            paths.partial_capture_binary, paths.partial_raw
        )
        environment = os.environ.copy()
        environment.update(
            {
                "LC_ALL": "C",
                "LANG": "C",
                "SIGROK_FIRMWARE_DIR": str(stage_directory),
                "SIGROK_FIRMWARE_PATH": "",
                "XDG_DATA_HOME": str(stage_directory / "xdg-home-empty"),
                "XDG_DATA_DIRS": str(stage_directory / "xdg-system-empty"),
            }
        )
        (stage_directory / "xdg-home-empty").mkdir()
        (stage_directory / "xdg-system-empty").mkdir()

        try:
            with paths.partial_capture_log.open("xb", buffering=0) as capture_log:
                setup_exit_status = run_child(
                    setup_command,
                    capture_log=capture_log,
                    environment=environment,
                    timeout_seconds=SETUP_TIMEOUT_SECONDS,
                )
                capture_log.flush()
                os.fsync(capture_log.fileno())
                if setup_exit_status != 0:
                    raise CaptureError(
                        f"sigrok-cli setup exited with status {setup_exit_status}"
                    )
                inspect_capture_log(
                    paths.partial_capture_log,
                    stage_directory,
                    expected_firmware_opens={
                        FIRMWARE_FILES[0].installed_name: 1,
                        FIRMWARE_FILES[1].installed_name: 1,
                    },
                    require_driver_receipt=False,
                )

                ready_read_fd, ready_write_fd = os.pipe2(os.O_CLOEXEC)
                stop_read_fd, stop_write_fd = os.pipe2(os.O_CLOEXEC)
                recorder_command = build_recorder_command(
                    paths.partial_recorder_binary,
                    paths.partial_template,
                    paths.partial_rig_evidence,
                    paths.partial_axis_characterization,
                    device,
                    paths.partial_locked_manifest,
                    paths.partial_session,
                    ready_write_fd,
                    stop_read_fd,
                )

                capture_started_unix_ms = time.time_ns() // 1_000_000
                capture_process = subprocess.Popen(
                    capture_command,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=capture_log,
                    env=environment,
                    start_new_session=True,
                )
                with paths.partial_recorder_log.open("xb", buffering=0) as recorder_log:
                    recorder_process = subprocess.Popen(
                        recorder_command,
                        stdin=subprocess.DEVNULL,
                        stdout=recorder_log,
                        stderr=subprocess.STDOUT,
                        env=os.environ.copy(),
                        pass_fds=(ready_write_fd, stop_read_fd),
                        start_new_session=True,
                    )
                    os.close(ready_write_fd)
                    ready_write_fd = -1
                    os.close(stop_read_fd)
                    stop_read_fd = -1

                    supervision = supervise_children(
                        capture_process=capture_process,
                        recorder_process=recorder_process,
                        raw_path=paths.partial_raw,
                        ready_read_fd=ready_read_fd,
                        stop_write_fd=stop_write_fd,
                        capture_log_fd=capture_log.fileno(),
                        capture_log_path=paths.partial_capture_log,
                        stage_directory=stage_directory,
                        recorder_log_fd=recorder_log.fileno(),
                    )
                    # supervise_children consumed the analyzer-success writer.
                    stop_write_fd = -1
                    capture_finished_unix_ms = time.time_ns() // 1_000_000
                    recorder_log.flush()
                    os.fsync(recorder_log.fileno())

                capture_log.flush()
                os.fsync(capture_log.fileno())
        except BaseException:
            # Kill the recorder before closing the guard. Otherwise a receipt
            # already in flight could misinterpret cleanup EOF as analyzer success.
            stop_child(recorder_process)
            stop_child(capture_process)
            raise
        finally:
            for descriptor in (
                ready_read_fd,
                ready_write_fd,
                stop_read_fd,
                stop_write_fd,
            ):
                if descriptor >= 0:
                    try:
                        os.close(descriptor)
                    except OSError:
                        pass

        if capture_finished_unix_ms <= capture_started_unix_ms:
            raise CaptureError("capture finish time is not later than its start time")

        session_bytes = verify_iks3_envelope(paths.partial_session)
        sync_regular_file(paths.partial_session)
        sync_regular_file(paths.partial_locked_manifest)
        locked_manifest_artifact = bound_artifact(
            paths.partial_locked_manifest, published_name=paths.locked_manifest.name
        )
        locked_manifest_sha256 = locked_manifest_artifact["sha256"]
        if not isinstance(locked_manifest_sha256, str):
            raise CaptureError("locked manifest SHA-256 was not computed")
        sync_regular_file(paths.partial_recorder_log)
        append_capture_receipts(
            paths.partial_capture_log,
            firmware_inventory,
            firmware_bundle_sha256,
            supervision,
            session_bytes,
            locked_manifest_sha256,
        )

    handoff = {
        "schema": HANDOFF_SCHEMA,
        "raw_capture": {"path": paths.raw.name, "bytes": EXPECTED_RAW_BYTES},
        "session_capture": {"path": paths.session.name, "bytes": session_bytes},
        "locked_manifest": locked_manifest_artifact,
        "word_bytes": RAW_WORD_BYTES,
        "byte_order": "little",
        "sample_rate_hz": SAMPLE_RATE_HZ,
        "channel_map": {
            "int1": [0, 1, 2, 3, 4, 5],
            "d8": 6,
            "d9": 7,
            "sda": 8,
            "scl": 9,
        },
        "voltage_threshold_low_mv": 1400,
        "voltage_threshold_high_mv": 3600,
        "requested_samples": REQUESTED_SAMPLES,
        "stop_window_first_byte": STOP_WINDOW_FIRST_BYTE,
        "stop_window_last_byte": STOP_WINDOW_LAST_BYTE,
        "stop_observed_raw_bytes": supervision.stop_observed_raw_bytes,
        "maximum_raw_poll_interval_ns": supervision.maximum_raw_poll_interval_ns,
        "capture_started_unix_ms": capture_started_unix_ms,
        "capture_finished_unix_ms": capture_finished_unix_ms,
        "capture_binary": bound_artifact(
            paths.partial_capture_binary, published_name=paths.capture_binary.name
        ),
        "recorder_binary": bound_artifact(
            paths.partial_recorder_binary, published_name=paths.recorder_binary.name
        ),
        "capture_log": bound_artifact(
            paths.partial_capture_log,
            include_hash=False,
            published_name=paths.capture_log.name,
        ),
        "recorder_log": bound_artifact(
            paths.partial_recorder_log,
            published_name=paths.recorder_log.name,
        ),
        "device_firmware_bundle": firmware_bundle_artifact,
        "acquisition_template": bound_artifact(
            paths.partial_template, published_name=paths.template.name
        ),
        "rig_evidence": bound_artifact(
            paths.partial_rig_evidence, published_name=paths.rig_evidence.name
        ),
        "axis_characterization": bound_artifact(
            paths.partial_axis_characterization,
            published_name=paths.axis_characterization.name,
        ),
        "driver_base_commit": DRIVER_BASE_COMMIT,
        "sigrok_cli_base_commit": SIGROK_CLI_BASE_COMMIT,
        "device_model": DEVICE_MODEL,
        "driver_patch": bound_artifact(
            paths.partial_driver_patch, published_name=paths.driver_patch.name
        ),
        "capture_exit_status": supervision.capture_exit_status,
        "recorder_exit_status": supervision.recorder_exit_status,
        "loaded_firmware": firmware_inventory,
        "setup_command": setup_command,
        "capture_command": capture_command,
        "recorder_command": recorder_command,
    }
    write_json_exclusive(paths.partial_handoff, handoff)

    # Recorder and analyzer outputs were synced above. Sync every remaining
    # partial and the directory before any public name can appear.
    for partial, _ in paths.publication_pairs():
        sync_regular_file(partial)
    fsync_directory(output_directory)
    publish_artifact_set(paths)
    return paths


def parse_arguments(arguments: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sigrok-cli", required=True, type=Path)
    parser.add_argument("--recorder", required=True, type=Path)
    parser.add_argument("--driver-patch", required=True, type=Path)
    parser.add_argument("--firmware-bundle", required=True, type=Path)
    parser.add_argument("--template", required=True, type=Path)
    parser.add_argument("--rig-evidence", required=True, type=Path)
    parser.add_argument("--axis-characterization", required=True, type=Path)
    parser.add_argument("--device", required=True)
    parser.add_argument("--output-directory", required=True, type=Path)
    parser.add_argument("--name", required=True)
    return parser.parse_args(arguments)


def main(arguments: Sequence[str] | None = None) -> int:
    options = parse_arguments(sys.argv[1:] if arguments is None else arguments)
    previous_handlers: dict[int, signal.Handlers] = {}

    def abort_from_signal(signum: int, _frame: object) -> None:
        # A second termination signal must not interrupt paired-child cleanup.
        for abort_signal in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
            signal.signal(abort_signal, signal.SIG_IGN)
        raise CaptureError(f"capture aborted by {signal.Signals(signum).name}")

    for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        previous_handlers[signum] = signal.signal(signum, abort_from_signal)
    try:
        paths = run_capture(
            sigrok_cli=options.sigrok_cli,
            recorder=options.recorder,
            driver_patch=options.driver_patch,
            firmware_bundle=options.firmware_bundle,
            template=options.template,
            rig_evidence=options.rig_evidence,
            axis_characterization=options.axis_characterization,
            device=options.device,
            output_directory=options.output_directory,
            stem=options.name,
        )
    except (CaptureError, OSError, subprocess.SubprocessError) as error:
        print(f"gate0a-capture: {error}", file=sys.stderr)
        return 1
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
    print(f"raw capture: {paths.raw}")
    print(f"IKS3 session: {paths.session}")
    print(f"locked manifest: {paths.locked_manifest}")
    print(f"Crabbox handoff: {paths.handoff}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
