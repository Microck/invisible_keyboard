#!/usr/bin/env python3
"""Process, pipe, publication, and pinned-stack tests for Gate 0A tooling."""

from __future__ import annotations

import importlib.util
import json
import os
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from collections.abc import Mapping, Sequence
from pathlib import Path
from types import ModuleType


TOOLS_DIRECTORY = Path(__file__).resolve().parent
REPOSITORY_ROOT = TOOLS_DIRECTORY.parent
REAL_CAPTURE_BINARY_ENV = "IKB_GATE0A_TEST_CAPTURE_BINARY"
FIRMWARE_BUNDLE_ENV = "IKB_GATE0A_TEST_FIRMWARE_BUNDLE"
CAPTURE_BINARY_RELATIVE_PATH = Path("artifacts/sigrok-cli-gate0a")
FIRMWARE_BUNDLE_RELATIVE_PATH = Path("artifacts/gate0a-dslogic-plus-firmware.tar")
EXPECTED_CAPTURE_BINARY_SHA256 = (
    "1665066921d1163bb2dddd45ae2ea20b55cf6970ac8de46ef5b0dbd2804e65c3"
)
EXPECTED_FIRMWARE_BUNDLE_SHA256 = (
    "bc85f870076a05aaa82393f341b2fca6b33b22deda92bf873e4979f0d645f8de"
)
EXPECTED_BUILD_INFO_SHA256 = (
    "0fcb2d7e6ebb9593173cbbbba74502a225111e3e0a2c97a684d5101f7c738268"
)
EXPECTED_PATCH_SHA256 = (
    "55d354597a5573315a16408112cf4dfbbda8976842b428c42681dbbc986bd10d"
)
PATCH_RECEIPT = b"IKB_DSLOGIC_TRANSFER_V1 completed_transfers=%lu anomalies=0"


def load_tool(module_name: str, filename: str) -> ModuleType:
    specification = importlib.util.spec_from_file_location(
        module_name, TOOLS_DIRECTORY / filename
    )
    if specification is None or specification.loader is None:
        raise RuntimeError(f"cannot load {filename}")
    module = importlib.util.module_from_spec(specification)
    # Dataclass resolves annotations through sys.modules during decoration.
    sys.modules[module_name] = module
    specification.loader.exec_module(module)
    return module


capture = load_tool("gate0a_capture", "capture-gate0a.py")
finalize = load_tool("gate0a_finalize", "finalize-gate0a-capture.py")


def required_test_runfile(setting_name: str) -> Path:
    logical_path = os.environ.get(setting_name)
    test_srcdir = os.environ.get("TEST_SRCDIR")
    if logical_path is None or test_srcdir is None:
        raise RuntimeError(f"missing Bazel runfile environment: {setting_name}")
    resolved_path = Path(test_srcdir) / logical_path
    if not resolved_path.is_file():
        raise RuntimeError(f"missing Bazel runfile: {logical_path}")
    return resolved_path


def required_external_file(
    environment: Mapping[str, str], setting_name: str
) -> Path:
    setting_value = environment.get(setting_name)
    if setting_value is None:
        raise RuntimeError(
            f"{setting_name} is required; invoke this manual target with Bazel --test_env"
        )
    path = Path(setting_value)
    if not path.is_absolute():
        raise RuntimeError(f"{setting_name} must be an absolute path")
    if path.is_symlink() or not path.is_file():
        raise RuntimeError(f"{setting_name} is not a regular non-symlink file")
    return path


def real_capture_stack_inputs(
    environment: Mapping[str, str],
) -> tuple[Path, Path, Path]:
    capture_binary = required_external_file(environment, REAL_CAPTURE_BINARY_ENV)
    if not os.access(capture_binary, os.X_OK):
        raise RuntimeError(f"{REAL_CAPTURE_BINARY_ENV} is not executable")
    if (
        capture_binary.parent.name != "artifacts"
        or capture_binary.name != CAPTURE_BINARY_RELATIVE_PATH.name
    ):
        raise RuntimeError(
            f"{REAL_CAPTURE_BINARY_ENV} must name {CAPTURE_BINARY_RELATIVE_PATH}"
        )
    build_root = capture_binary.parent.parent
    firmware_bundle = required_external_file(environment, FIRMWARE_BUNDLE_ENV)
    if firmware_bundle != build_root / FIRMWARE_BUNDLE_RELATIVE_PATH:
        raise RuntimeError("capture binary and firmware must come from one pinned build")
    return capture_binary, firmware_bundle, build_root


def load_generated_sha256sums(build_root: Path) -> dict[str, str]:
    checksum_path = build_root / "SHA256SUMS"
    if checksum_path.is_symlink() or not checksum_path.is_file():
        raise RuntimeError("pinned build is missing its regular SHA256SUMS file")
    checksums: dict[str, str] = {}
    for line_number, line in enumerate(
        checksum_path.read_text(encoding="ascii").splitlines(), start=1
    ):
        digest, separator, member = line.partition("  ")
        member_path = Path(member)
        if (
            separator != "  "
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
            or not member
            or member_path.is_absolute()
            or ".." in member_path.parts
        ):
            raise RuntimeError(f"malformed SHA256SUMS line {line_number}")
        if member in checksums:
            raise RuntimeError(f"duplicate SHA256SUMS member: {member}")
        checksums[member] = digest
    return checksums


def require_manifested_artifact(
    build_root: Path, checksums: Mapping[str, str], relative_path: Path
) -> tuple[Path, str]:
    member = relative_path.as_posix()
    expected_sha256 = checksums.get(member)
    if expected_sha256 is None:
        raise RuntimeError(f"SHA256SUMS is missing {member}")
    artifact = build_root / relative_path
    if artifact.is_symlink() or not artifact.is_file():
        raise RuntimeError(f"pinned build artifact is missing: {member}")
    actual_sha256 = capture.sha256_file(artifact)
    if actual_sha256 != expected_sha256:
        raise RuntimeError(f"SHA-256 mismatch for pinned build artifact: {member}")
    return artifact, actual_sha256


def file_contains(path: Path, needle: bytes) -> bool:
    overlap = b""
    with path.open("rb") as source:
        while chunk := source.read(64 * 1024):
            candidate = overlap + chunk
            if needle in candidate:
                return True
            overlap = candidate[-(len(needle) - 1) :]
    return False


def verify_real_capture_stack(
    capture_binary: Path, firmware_bundle: Path, build_root: Path
) -> str:
    checksums = load_generated_sha256sums(build_root)
    manifested_binary, binary_sha256 = require_manifested_artifact(
        build_root, checksums, CAPTURE_BINARY_RELATIVE_PATH
    )
    if manifested_binary != capture_binary:
        raise RuntimeError("explicit capture binary differs from manifested artifact")
    if binary_sha256 != EXPECTED_CAPTURE_BINARY_SHA256:
        raise RuntimeError("capture binary SHA-256 does not match canonical build")
    manifested_firmware, firmware_sha256 = require_manifested_artifact(
        build_root, checksums, FIRMWARE_BUNDLE_RELATIVE_PATH
    )
    if manifested_firmware != firmware_bundle:
        raise RuntimeError("explicit firmware bundle differs from manifested artifact")
    if firmware_sha256 != EXPECTED_FIRMWARE_BUNDLE_SHA256:
        raise RuntimeError("firmware bundle SHA-256 does not match canonical build")

    patch, patch_sha256 = require_manifested_artifact(
        build_root, checksums, Path("artifacts/libsigrok-gate0a.patch")
    )
    if patch_sha256 != EXPECTED_PATCH_SHA256:
        raise RuntimeError("driver patch SHA-256 does not match canonical build")
    if patch.read_bytes() != required_test_runfile("IKB_GATE0A_PATCH_RLOCATION").read_bytes():
        raise RuntimeError("built driver patch differs from the declared runfile")
    provenance, _ = require_manifested_artifact(
        build_root, checksums, Path("artifacts/capture-stack-provenance.md")
    )
    if provenance.read_bytes() != required_test_runfile(
        "IKB_GATE0A_PROVENANCE_RLOCATION"
    ).read_bytes():
        raise RuntimeError("built provenance differs from the declared runfile")
    build_info, build_info_sha256 = require_manifested_artifact(
        build_root, checksums, Path("artifacts/BUILD-INFO.txt")
    )
    if build_info_sha256 != EXPECTED_BUILD_INFO_SHA256:
        raise RuntimeError("BUILD-INFO SHA-256 does not match canonical build")
    build_info_text = build_info.read_text(encoding="utf-8")
    for expected_line in (
        "libsigrok_commit=0bc2487778e660f4d3116729b6f4aee2b1996bb0",
        "sigrok_cli_commit=f44dd91347e7ac797cefc23162b9fcf0b7329f1f",
        "host=x86_64-linux",
        "byte_order=little",
        f"patch_sha256={EXPECTED_PATCH_SHA256}",
    ):
        if expected_line not in build_info_text.splitlines():
            raise RuntimeError(f"BUILD-INFO is missing pin: {expected_line}")
    if "libsigrok" in build_info_text.split("ELF needed libraries:\n", 1)[-1]:
        raise RuntimeError("capture binary retains a dynamic libsigrok dependency")
    version_log, _ = require_manifested_artifact(
        build_root, checksums, Path("logs/sigrok-cli-version.log")
    )
    if not file_contains(capture_binary, PATCH_RECEIPT):
        raise RuntimeError("capture binary lacks the patched transfer receipt")
    version = subprocess.run(
        [str(capture_binary), "--version"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
        timeout=10,
    )
    if version.returncode != 0:
        raise RuntimeError(
            f"real capture binary --version failed with status {version.returncode}"
        )
    if version.stdout != version_log.read_bytes() or b"sigrok-cli" not in version.stdout:
        raise RuntimeError("real capture binary version output differs from build receipt")
    return binary_sha256


def write_executable(path: Path, source: str) -> Path:
    path.write_text(textwrap.dedent(source).lstrip("\n"), encoding="utf-8")
    path.chmod(0o755)
    return path


def fixture_sigrok(path: Path, mode: str, log_fault: str | None = None) -> Path:
    if mode not in {"success", "jump", "analyzer-failure", "wait", "late-boot", "ready-edge"}:
        raise ValueError(mode)
    receipt = "IKB_DSLOGIC_TRANSFER_V1 completed_transfers=361000 anomalies=0"
    final_log = {
        None: f"print({receipt!r}, file=sys.stderr)",
        "missing-receipt": "",
        "duplicate-receipt": f"print({(receipt + chr(10) + receipt)!r}, file=sys.stderr)",
        "failure-marker": "print('IKB_DSLOGIC_TRANSFER_FAILURE_V1', file=sys.stderr)",
        "firmware-open-count": (
            f"print({receipt!r}, file=sys.stderr)\n"
            "print(f\"Opened '{fpga}'.\", file=sys.stderr)"
        ),
        "final-log-overflow": (
            f"os.ftruncate(sys.stderr.fileno(), {capture.MAX_CAPTURE_CHILD_LOG_BYTES + 1})"
        ),
    }[log_fault]
    action = {
        "success": f"""
            raw.truncate({capture.STOP_WINDOW_FIRST_BYTE + 500_000})
            raw.flush(); os.fsync(raw.fileno())
            time.sleep(0.05)
            raw.truncate({capture.EXPECTED_RAW_BYTES})
            raw.flush(); os.fsync(raw.fileno())
        """,
        "jump": f"""
            raw.truncate({capture.STOP_WINDOW_LAST_BYTE + 1})
            raw.flush(); os.fsync(raw.fileno())
            time.sleep(10)
        """,
        "analyzer-failure": f"""
            raw.truncate({capture.STOP_WINDOW_FIRST_BYTE})
            raw.flush(); os.fsync(raw.fileno())
            time.sleep(0.03)
            raise SystemExit(7)
        """,
        "wait": "time.sleep(10)\n",
        "late-boot": "raw.truncate(12_000_001); raw.flush(); time.sleep(10)\n",
        "ready-edge": f"""
            raw.truncate(12_000_000); raw.flush()
            time.sleep(0.3)
            raw.truncate({capture.STOP_WINDOW_FIRST_BYTE + 500_000}); raw.flush()
            time.sleep(0.05)
            raw.truncate({capture.EXPECTED_RAW_BYTES}); raw.flush()
        """,
    }[mode]
    return write_executable(
        path,
        f"""
        #!/usr/bin/env python3
        import os
        import sys
        import time

        firmware = os.environ["SIGROK_FIRMWARE_DIR"]
        fpga = os.path.join(firmware, "{capture.FIRMWARE_FILES[0].installed_name}")
        fx2 = os.path.join(firmware, "{capture.FIRMWARE_FILES[1].installed_name}")
        if "--show" in sys.argv:
            print(f"Opened '{{fpga}}'.", file=sys.stderr)
            print(f"Opened '{{fx2}}'.", file=sys.stderr)
            raise SystemExit(0)
        output = sys.argv[sys.argv.index("--output-file") + 1]
        with open(output, "xb") as raw:
            raw.truncate(1_000_000)
            raw.flush(); os.fsync(raw.fileno())
            time.sleep(0.05)
{textwrap.indent(textwrap.dedent(action).strip(), '            ')}
        print(f"Opened '{{fpga}}'.", file=sys.stderr)
{textwrap.indent(final_log, '        ')}
        """,
    )


def fixture_recorder(path: Path, mode: str) -> Path:
    if mode not in {"success", "recorder-failure", "bad-readiness", "log-overflow", "late-readiness"}:
        raise ValueError(mode)
    terminal = {
        "success": """
            if os.read(stop_fd, 1) != b"\\x01":
                raise SystemExit(21)
            if os.read(stop_fd, 1) != b"":
                raise SystemExit(22)
            time.sleep(0.1)
            with open(session, "ab", buffering=0) as output:
                output.write(b"END3" + bytes(12))
                output.flush(); os.fsync(output.fileno())
            print("stop=01 guard=eof silence_ms=100")
        """,
        "recorder-failure": "raise SystemExit(9)\n",
        "bad-readiness": "time.sleep(10)\n",
        "log-overflow": f"""
            os.ftruncate(sys.stdout.fileno(), {capture.MAX_RECORDER_LOG_BYTES + 1})
            os.fsync(sys.stdout.fileno())
            time.sleep(10)
        """,
    }["success" if mode == "late-readiness" else mode]
    readiness = "b'\\x01\\x01'" if mode == "bad-readiness" else "b'\\x01'"
    return write_executable(
        path,
        f"""
        #!/usr/bin/env python3
        import hashlib
        import json
        import os
        import sys
        import time

        if sys.argv[1] == "score-gate0a":
            manifest, session, sidecar, report = sys.argv[2], sys.argv[5], sys.argv[7], sys.argv[8]
            def sha(path):
                digest = hashlib.sha256()
                with open(path, "rb") as source:
                    while chunk := source.read(1024 * 1024):
                        digest.update(chunk)
                return digest.hexdigest()
            with open(sidecar, encoding="utf-8") as source:
                sidecar_value = json.load(source)
            value = {{
                "schema": "{finalize.REPORT_SCHEMA}",
                "source_manifest_sha256": sha(manifest),
                "source_session_sha256": sha(session),
                "raw_capture_sha256": sidecar_value["raw_capture_sha256"],
                "capture_sidecar_sha256": sha(sidecar),
                "criteria": {{"deterministic_quiesce": "pass"}},
                "trace": {{
                    "schema": "{finalize.TRACE_SCHEMA}",
                    "metrics": {{
                        "final_sequence": 7,
                        "final_d8_rise_sample": 100,
                        "final_d8_fall_sample": 110,
                        "shutdown_unadmitted_int1_edges": [0, 0, 0, 0, 0, 0],
                        "shutdown_transactions": 52,
                        "shutdown_scl_rising_edges": 1422,
                        "terminal_marker_rise_sample": 200,
                        "terminal_marker_fall_sample": 400,
                        "terminal_marker_width_samples": 200,
                        "terminal_idle_samples": 10000000,
                        "host_response_time_ns": 100000000,
                        "deterministic_quiesce_violations": 0,
                    }},
                }},
            }}
            with open(report, "x", encoding="utf-8") as output:
                json.dump(value, output, sort_keys=True)
                output.write("\\n")
            raise SystemExit(0)

        time.sleep({0.2 if mode == "late-readiness" else 0})
        manifest = sys.argv[6]
        session = sys.argv[7]
        ready_fd = int(sys.argv[sys.argv.index("--ready-fd") + 1])
        stop_fd = int(sys.argv[sys.argv.index("--stop-fd") + 1])
        with open(manifest, "xb", buffering=0) as output:
            output.write(b'{{"schema":"fixture"}}\\n')
            output.flush(); os.fsync(output.fileno())
        with open(session, "xb", buffering=0) as output:
            output.write(b"IKS3\\x03" + bytes(59))
            output.flush(); os.fsync(output.fileno())
        os.write(ready_fd, {readiness})
        os.close(ready_fd)
{textwrap.indent(textwrap.dedent(terminal).strip(), '        ')}
        """,
    )


def fixture_failing_scorer(path: Path, exit_status: int) -> Path:
    return write_executable(
        path,
        f"""
        #!/usr/bin/env python3
        import sys

        print("fixture scorer failure", file=sys.stderr)
        raise SystemExit({exit_status})
        """,
    )


def fixture_distinct_scorer(path: Path) -> Path:
    return write_executable(
        path,
        """
        #!/usr/bin/env python3
        raise SystemExit("distinct scorer must not run")
        """,
    )


def fixture_noisy_scorer(path: Path) -> Path:
    return write_executable(
        path,
        f"""
        #!/usr/bin/env python3
        import os
        import sys
        import time

        os.ftruncate(sys.stderr.fileno(), {finalize.MAX_SCORER_STDERR_BYTES + 1})
        os.fsync(sys.stderr.fileno())
        time.sleep(10)
        """,
    )


def fixture_replacement_scorer(path: Path, execution_marker: Path) -> Path:
    return write_executable(
        path,
        f"""
        #!/usr/bin/env python3
        from pathlib import Path

        Path({str(execution_marker)!r}).write_text("replacement executed\\n", encoding="utf-8")
        raise SystemExit(91)
        """,
    )


def run_admitted_scorer(command: Sequence[str]) -> None:
    scorer = Path(command[0])
    scorer_metadata = scorer.stat()
    executable_fd = finalize.admit_scorer_image(
        scorer,
        expected_bytes=scorer_metadata.st_size,
        expected_sha256=capture.sha256_file(scorer),
    )
    try:
        finalize.run_scorer(command, executable_fd=executable_fd)
    finally:
        os.close(executable_fd)


def replace_scorer_when_memfd_appears(
    scorer: Path,
    replacement: Path,
    sidecar_partial: Path,
) -> subprocess.Popen[str]:
    watcher = """
        import os
        import sys
        import time
        from pathlib import Path

        parent_pid = int(sys.argv[1])
        replacement = sys.argv[2]
        scorer = sys.argv[3]
        sidecar_partial = Path(sys.argv[4])
        scorer_memfd = sys.argv[5]
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                for descriptor in Path(f"/proc/{parent_pid}/fd").iterdir():
                    try:
                        target = os.readlink(descriptor)
                    except OSError:
                        continue
                    if scorer_memfd not in target:
                        continue
                    os.replace(replacement, scorer)
                    if sidecar_partial.exists():
                        print("sidecar existed before scorer replacement", file=sys.stderr)
                        raise SystemExit(2)
                    raise SystemExit(0)
            except FileNotFoundError:
                break
            time.sleep(0.01)
        print("admitted scorer descriptor was not observed", file=sys.stderr)
        raise SystemExit(3)
    """
    return subprocess.Popen(
        [
            sys.executable,
            "-c",
            textwrap.dedent(watcher),
            str(os.getpid()),
            str(replacement),
            str(scorer),
            str(sidecar_partial),
            f"memfd:{finalize.SCORER_MEMFD_NAME}",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def fixture_inputs(directory: Path) -> dict[str, Path]:
    inputs: dict[str, Path] = {}
    for name in ("template", "rig", "axis"):
        path = directory / f"{name}.json"
        path.write_text(f'{{"fixture":"{name}"}}\n', encoding="utf-8")
        inputs[name] = path
    return inputs


def firmware_bundle() -> Path:
    return required_external_file(os.environ, FIRMWARE_BUNDLE_ENV)


def wrapper_arguments(
    directory: Path,
    *,
    sigrok_mode: str,
    recorder_mode: str,
    stem: str,
    log_fault: str | None = None,
) -> tuple[list[str], Path]:
    output = directory / "output"
    output.mkdir()
    inputs = fixture_inputs(directory)
    sigrok = fixture_sigrok(directory / "sigrok-fixture", sigrok_mode, log_fault)
    recorder = fixture_recorder(directory / "recorder-fixture", recorder_mode)
    arguments = [
        "--sigrok-cli",
        str(sigrok),
        "--recorder",
        str(recorder),
        "--driver-patch",
        str(REPOSITORY_ROOT / "third_party/libsigrok-0bc2487-dslogic-gate0a.patch"),
        "--firmware-bundle",
        str(firmware_bundle()),
        "--template",
        str(inputs["template"]),
        "--rig-evidence",
        str(inputs["rig"]),
        "--axis-characterization",
        str(inputs["axis"]),
        "--device",
        "/dev/fixture-controller",
        "--output-directory",
        str(output),
        "--name",
        stem,
    ]
    return arguments, output


def run_wrapper_arguments(arguments: Sequence[str]) -> capture.OutputPaths:
    options = capture.parse_arguments(arguments)
    return capture.run_capture(
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


class RealCaptureBinaryContractTests(unittest.TestCase):
    def test_missing_real_capture_binary_environment_fails_closed(self) -> None:
        with self.assertRaisesRegex(RuntimeError, f"^{REAL_CAPTURE_BINARY_ENV} is required"):
            real_capture_stack_inputs({FIRMWARE_BUNDLE_ENV: "/tmp/unused"})

    def test_real_pinned_capture_binary_is_hash_bound_and_loadable(self) -> None:
        capture_binary, bundle, build_root = real_capture_stack_inputs(os.environ)
        self.assertEqual(
            verify_real_capture_stack(capture_binary, bundle, build_root),
            EXPECTED_CAPTURE_BINARY_SHA256,
        )


class CaptureContractTests(unittest.TestCase):

    def test_production_wrapper_invocation_publishes_one_complete_set(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            arguments, output = wrapper_arguments(
                directory,
                sigrok_mode="success",
                recorder_mode="success",
                stem="paired-success",
            )
            completed = subprocess.run(
                [sys.executable, str(TOOLS_DIRECTORY / "capture-gate0a.py"), *arguments],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
                timeout=20,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            paths = capture.output_paths(output, "paired-success")
            self.assertTrue(all(final.is_file() for _, final in paths.publication_pairs()))
            self.assertTrue(all(not partial.exists() for partial, _ in paths.publication_pairs()))
            self.assertEqual(paths.raw.stat().st_size, capture.EXPECTED_RAW_BYTES)
            self.assertLess(paths.raw.stat().st_blocks * 512, 2 * 1024 * 1024)
            self.assertIn(b"stop=01 guard=eof silence_ms=100", paths.recorder_log.read_bytes())
            handoff = json.loads(paths.handoff.read_text(encoding="utf-8"))
            self.assertEqual(handoff["schema"], capture.HANDOFF_SCHEMA)
            self.assertNotIn("sha256", handoff["raw_capture"])
            self.assertNotIn("sha256", handoff["session_capture"])
            self.assertGreaterEqual(
                handoff["stop_observed_raw_bytes"], capture.STOP_WINDOW_FIRST_BYTE
            )
            self.assertLessEqual(
                handoff["stop_observed_raw_bytes"], capture.STOP_WINDOW_LAST_BYTE
            )
            self.assertLessEqual(
                handoff["maximum_raw_poll_interval_ns"],
                capture.MAX_RAW_POLL_INTERVAL_NS,
            )

    def assert_failed_pair(
        self, sigrok_mode: str, recorder_mode: str, message: str,
        *, log_fault: str | None = None,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            arguments, output = wrapper_arguments(
                directory,
                sigrok_mode=sigrok_mode,
                recorder_mode=recorder_mode,
                stem="paired-failure",
                log_fault=log_fault,
            )
            completed = subprocess.run(
                [sys.executable, str(TOOLS_DIRECTORY / "capture-gate0a.py"), *arguments],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
                timeout=20,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn(message, completed.stderr)
            paths = capture.output_paths(output, "paired-failure")
            self.assertTrue(all(not final.exists() for _, final in paths.publication_pairs()))
            self.assertTrue(any(partial.exists() for partial, _ in paths.publication_pairs()))
            if log_fault is not None:
                self.assertEqual(paths.partial_raw.stat().st_size, capture.EXPECTED_RAW_BYTES)
                self.assertNotIn(b"END3", paths.partial_session.read_bytes())

    def test_invalid_final_analyzer_log_never_authorizes_a_recorder_footer(self) -> None:
        for log_fault, message in (
            ("missing-receipt", "expected 1 patched-driver receipt(s), found 0"),
            ("duplicate-receipt", "expected 1 patched-driver receipt(s), found 2"),
            ("failure-marker", "capture log contains failure marker"),
            ("firmware-open-count", "to be opened 2 time(s), found 3"),
            ("final-log-overflow", "sigrok-cli log exceeded"),
        ):
            with self.subTest(log_fault=log_fault):
                self.assert_failed_pair("success", "success", message, log_fault=log_fault)

    def test_chunk_jump_beyond_stop_window_suppresses_publication(self) -> None:
        self.assert_failed_pair("jump", "success", "stop window ends")

    def test_analyzer_failure_aborts_recorder_and_suppresses_publication(self) -> None:
        self.assert_failed_pair("analyzer-failure", "success", "status 7")

    def test_recorder_failure_aborts_analyzer_and_suppresses_publication(self) -> None:
        self.assert_failed_pair("wait", "recorder-failure", "recorder exited with status 9")

    def test_readiness_requires_one_byte_followed_by_eof(self) -> None:
        self.assert_failed_pair("wait", "bad-readiness", "readiness must be exactly")

    def test_readiness_after_the_boot_window_suppresses_publication(self) -> None:
        self.assert_failed_pair("late-boot", "late-readiness", "boot window")

    def test_readiness_at_the_boot_window_boundary_preserves_the_full_capture(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            arguments, output = wrapper_arguments(
                Path(temporary), sigrok_mode="ready-edge", recorder_mode="late-readiness",
                stem="readiness-boundary",
            )
            paths = run_wrapper_arguments(arguments)
            self.assertEqual(paths.raw.stat().st_size, capture.EXPECTED_RAW_BYTES)
            self.assertIn(b"stop=01 guard=eof silence_ms=100", paths.recorder_log.read_bytes())

    def test_recorder_log_quota_aborts_both_children(self) -> None:
        self.assert_failed_pair("wait", "log-overflow", "recorder log exceeded")

    def test_signal_aborts_both_children_without_publication(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            arguments, output = wrapper_arguments(
                directory,
                sigrok_mode="wait",
                recorder_mode="success",
                stem="signal-abort",
            )
            process = subprocess.Popen(
                [sys.executable, str(TOOLS_DIRECTORY / "capture-gate0a.py"), *arguments],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            paths = capture.output_paths(output, "signal-abort")
            deadline = time.monotonic() + 5
            while not paths.partial_session.exists() and time.monotonic() < deadline:
                time.sleep(0.005)
            self.assertTrue(paths.partial_session.exists(), "recorder never became ready")
            process.send_signal(signal.SIGTERM)
            _, stderr = process.communicate(timeout=15)
            self.assertNotEqual(process.returncode, 0)
            self.assertIn("SIGTERM", stderr)
            self.assertTrue(all(not final.exists() for _, final in paths.publication_pairs()))

    def test_publication_rolls_back_finals_and_keeps_all_partials(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            paths = capture.output_paths(directory, "race")
            for partial, _ in paths.publication_pairs():
                partial.write_bytes(b"partial")
            collision = paths.firmware_bundle
            collision.write_bytes(b"outside")
            with self.assertRaises(FileExistsError):
                capture.publish_artifact_set(paths)
            self.assertEqual(collision.read_bytes(), b"outside")
            self.assertTrue(all(partial.exists() for partial, _ in paths.publication_pairs()))
            self.assertTrue(
                all(not final.exists() for _, final in paths.publication_pairs() if final != collision)
            )


class FinalizationContractTests(unittest.TestCase):
    def test_log_binds_driver_stop_session_manifest_and_firmware(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "capture.log"
            firmware_hash = "a" * 64
            manifest_hash = "b" * 64
            lines = [
                b"IKB_DSLOGIC_TRANSFER_V1 completed_transfers=42000 anomalies=0\n"
            ]
            for firmware in finalize.FIRMWARE_INVENTORY:
                lines.append(
                    (
                        "IKB_GATE0_FIRMWARE_V1 "
                        f"name={firmware['name']} bytes={firmware['bytes']} "
                        f"sha256={firmware['sha256']}\n"
                    ).encode("ascii")
                )
            lines.append(
                (
                    "IKB_GATE0_CAPTURE_V2 samples=7220000000 "
                    "completed_transfers=42000 anomalies=0 "
                    "stop_observed_raw_bytes=14416000000 session_bytes=80 "
                    f"locked_manifest_sha256={manifest_hash} "
                    f"firmware_bundle_sha256={firmware_hash}\n"
                ).encode("ascii")
            )
            log.write_bytes(b"".join(lines))
            completed_transfers, log_sha256 = finalize.inspect_final_log(
                log,
                expected_firmware_bundle_sha256=firmware_hash,
                expected_stop_observed_raw_bytes=14_416_000_000,
                expected_session_bytes=80,
                expected_locked_manifest_sha256=manifest_hash,
            )
            self.assertEqual(completed_transfers, 42_000)
            self.assertEqual(log_sha256, capture.sha256_file(log))

    def test_crabbox_finalizer_hashes_large_inputs_and_resists_scorer_replacement(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            arguments, output = wrapper_arguments(
                directory,
                sigrok_mode="success",
                recorder_mode="success",
                stem="finalize",
            )
            paths = run_wrapper_arguments(arguments)
            sidecar = output / "finalize.sidecar.json"
            report = output / "finalize.report.json"
            sidecar_partial = output / ".finalize.sidecar.json.partial"
            execution_marker = directory / "replacement-executed"
            replacement = fixture_replacement_scorer(
                directory / "replacement-scorer", execution_marker
            )
            replacement_sha256 = capture.sha256_file(replacement)
            watcher = replace_scorer_when_memfd_appears(
                paths.recorder_binary, replacement, sidecar_partial
            )
            previous = os.environ.get("IKB_CRABBOX_FINALIZE")
            os.environ["IKB_CRABBOX_FINALIZE"] = "1"
            try:
                sidecar_value, report_value, session_sha256 = finalize.finalize(
                    paths.handoff, sidecar, report, paths.recorder_binary
                )
                _, watcher_stderr = watcher.communicate(timeout=20)
            finally:
                if watcher.poll() is None:
                    watcher.terminate()
                    watcher.communicate(timeout=10)
                if previous is None:
                    os.environ.pop("IKB_CRABBOX_FINALIZE", None)
                else:
                    os.environ["IKB_CRABBOX_FINALIZE"] = previous
            self.assertEqual(watcher.returncode, 0, watcher_stderr)
            self.assertEqual(
                capture.sha256_file(paths.recorder_binary), replacement_sha256
            )
            self.assertFalse(execution_marker.exists())
            self.assertTrue(sidecar.is_file())
            self.assertTrue(report.is_file())
            self.assertEqual(session_sha256, capture.sha256_file(paths.session))
            self.assertEqual(
                report_value["raw_capture_sha256"],
                sidecar_value["raw_capture_sha256"],
            )
            self.assertEqual(
                report_value["capture_sidecar_sha256"], capture.sha256_file(sidecar)
            )
            self.assertFalse(sidecar_partial.exists())
            self.assertFalse((output / ".finalize.report.json.partial").exists())

    def test_scorer_failure_is_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            scorer = fixture_failing_scorer(Path(temporary) / "scorer", 17)
            with self.assertRaisesRegex(finalize.FinalizeError, "status 17"):
                run_admitted_scorer([str(scorer)])

    def test_scorer_stderr_quota_is_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            scorer = fixture_noisy_scorer(Path(temporary) / "noisy-scorer")
            with self.assertRaisesRegex(finalize.FinalizeError, "stderr exceeded"):
                run_admitted_scorer([str(scorer)])

    def test_finalizer_rejects_a_distinct_scorer_identity(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            arguments, output = wrapper_arguments(
                directory,
                sigrok_mode="success",
                recorder_mode="success",
                stem="wrong-scorer",
            )
            paths = run_wrapper_arguments(arguments)
            distinct_scorer = fixture_distinct_scorer(directory / "distinct-scorer")
            previous = os.environ.get("IKB_CRABBOX_FINALIZE")
            os.environ["IKB_CRABBOX_FINALIZE"] = "1"
            try:
                with self.assertRaisesRegex(
                    finalize.FinalizeError, "wrapper-published recorder"
                ):
                    finalize.finalize(
                        paths.handoff,
                        output / "wrong-scorer.sidecar.json",
                        output / "wrong-scorer.report.json",
                        distinct_scorer,
                    )
            finally:
                if previous is None:
                    os.environ.pop("IKB_CRABBOX_FINALIZE", None)
                else:
                    os.environ["IKB_CRABBOX_FINALIZE"] = previous
            self.assertFalse((output / "wrong-scorer.sidecar.json").exists())
            self.assertFalse((output / "wrong-scorer.report.json").exists())

    def test_final_sidecar_and_report_publish_as_a_pair(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            sidecar_partial = directory / ".sidecar.partial"
            report_partial = directory / ".report.partial"
            sidecar = directory / "sidecar"
            report = directory / "report"
            sidecar_partial.write_bytes(b"sidecar")
            report_partial.write_bytes(b"report")
            report.write_bytes(b"collision")
            with self.assertRaises(FileExistsError):
                finalize.publish_pair(sidecar_partial, sidecar, report_partial, report)
            self.assertFalse(sidecar.exists())
            self.assertEqual(report.read_bytes(), b"collision")
            self.assertTrue(sidecar_partial.exists())
            self.assertTrue(report_partial.exists())


class StaticCaptureStackContractTests(unittest.TestCase):

    def test_bazel_build_entrypoint_resolves_declared_inputs(self) -> None:
        build_entrypoint = required_test_runfile("IKB_GATE0A_BUILD_RLOCATION")
        environment = os.environ.copy()
        environment.pop("IKB_CRABBOX_BUILD", None)
        attempted_build = subprocess.run(
            [build_entrypoint],
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(attempted_build.returncode, 1)
        self.assertIn(
            "refusing to compile outside the explicit Crabbox build environment",
            attempted_build.stderr,
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
