#!/usr/bin/env python3
"""Exercise hardware-mode cue receipts through real Chrome and real TCP.

The Rust fixture uses synthetic typed runner events to reach held-out
validation. This contract establishes browser, HTTP, and response-loss
behavior only. It does not establish sensor or passive-surface feasibility.
"""

from __future__ import annotations

import base64
import json
import os
import selectors
import shutil
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


FIXTURE_RUNFILE_ENV = "IKB_CALIBRATION_BROWSER_FIXTURE_RLOCATION"
MAX_HTTP_MESSAGE_BYTES = 2 * 1024 * 1024


class BrowserContractError(RuntimeError):
    """The synthetic real-browser contract did not establish its claim."""


@dataclass(frozen=True)
class AcknowledgementAttempt:
    revision: int
    status: int
    disposition: str


class ProxyEvidence:
    """Thread-safe receipt-attempt log plus the first-response fault gate."""

    def __init__(self) -> None:
        self.changed = threading.Condition()
        self.attempts: list[AcknowledgementAttempt] = []
        self.first_accepted_response_held = False
        self.release_first_response = False
        self.fatal_error: str | None = None

    def record_response(self, revision: int, status: int) -> bool:
        """Record an acknowledgement and reserve the first accepted response."""
        with self.changed:
            hold_response = status == 200 and not self.first_accepted_response_held
            if hold_response:
                self.first_accepted_response_held = True
            self.attempts.append(
                AcknowledgementAttempt(
                    revision,
                    status,
                    "held-then-dropped" if hold_response else "forwarded",
                )
            )
            self.changed.notify_all()
            return hold_response

    def wait_for_first_accepted(self, timeout: float = 5.0) -> None:
        with self.changed:
            reached_boundary = self.changed.wait_for(
                lambda: self.first_accepted_response_held or self.fatal_error is not None,
                timeout,
            )
            self._raise_fatal_locked()
            if not reached_boundary or not self.first_accepted_response_held:
                raise BrowserContractError(
                    "proxy did not observe an accepted surface acknowledgement"
                )

    def wait_until_released(self) -> None:
        with self.changed:
            self.changed.wait_for(
                lambda: self.release_first_response or self.fatal_error is not None
            )
            self._raise_fatal_locked()

    def release(self) -> None:
        with self.changed:
            self.release_first_response = True
            self.changed.notify_all()

    def fail(self, error: BaseException) -> None:
        with self.changed:
            if self.fatal_error is None:
                self.fatal_error = str(error)
            self.changed.notify_all()

    def snapshot(self) -> list[AcknowledgementAttempt]:
        with self.changed:
            self._raise_fatal_locked()
            return list(self.attempts)

    def _raise_fatal_locked(self) -> None:
        if self.fatal_error is not None:
            raise BrowserContractError(f"receipt proxy failed: {self.fatal_error}")


class ReceiptProxyServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(
        self,
        backend: tuple[str, int],
        backend_origin: str,
        evidence: ProxyEvidence,
    ) -> None:
        self.backend = backend
        self.backend_origin = backend_origin
        self.evidence = evidence
        super().__init__(("127.0.0.1", 0), ReceiptProxyHandler)


class ReceiptProxyHandler(socketserver.BaseRequestHandler):
    server: ReceiptProxyServer

    def handle(self) -> None:
        try:
            request = read_http_message(self.request)
            acknowledgement_revision = surface_acknowledgement_revision(request)
            forwarded_request = rewrite_origin(request, self.server.backend_origin)
            with socket.create_connection(self.server.backend, timeout=5) as upstream:
                upstream.sendall(forwarded_request)
                response = read_http_message(upstream)

            if acknowledgement_revision is not None:
                status = response_status(response)
                if self.server.evidence.record_response(
                    acknowledgement_revision, status
                ):
                    # The production service has accepted the action. Hold the
                    # browser fetch open so a replacement and newer cue can be
                    # published before this connection is closed with no bytes.
                    self.server.evidence.wait_until_released()
                    return
            self.request.sendall(response)
        except (BrowserContractError, OSError, UnicodeError, ValueError) as error:
            self.server.evidence.fail(error)


def read_http_message(connection: socket.socket) -> bytes:
    connection.settimeout(10)
    message = bytearray()
    expected_size: int | None = None
    while expected_size is None or len(message) < expected_size:
        chunk = connection.recv(64 * 1024)
        if not chunk:
            if expected_size is None or len(message) != expected_size:
                raise BrowserContractError("TCP peer closed before one HTTP message")
            break
        message.extend(chunk)
        if len(message) > MAX_HTTP_MESSAGE_BYTES:
            raise BrowserContractError("HTTP message exceeded the fixture limit")
        if expected_size is None:
            header_end = message.find(b"\r\n\r\n")
            if header_end >= 0:
                content_length = parse_content_length(bytes(message[:header_end]))
                expected_size = header_end + 4 + content_length
    if expected_size is None:
        raise BrowserContractError("HTTP message did not contain complete headers")
    return bytes(message[:expected_size])


def parse_content_length(headers: bytes) -> int:
    content_length = 0
    for line in headers.decode("iso-8859-1").split("\r\n")[1:]:
        name, separator, value = line.partition(":")
        if separator and name.strip().lower() == "content-length":
            content_length = int(value.strip())
    return content_length


def rewrite_origin(request: bytes, backend_origin: str) -> bytes:
    header_end = request.find(b"\r\n\r\n")
    if header_end < 0:
        raise BrowserContractError("cannot rewrite an HTTP request without headers")
    lines = request[:header_end].decode("iso-8859-1").split("\r\n")
    rewritten: list[str] = []
    saw_connection = False
    for line in lines:
        name, separator, _ = line.partition(":")
        lowered = name.strip().lower()
        if separator and lowered == "origin":
            rewritten.append(f"Origin: {backend_origin}")
        elif separator and lowered == "connection":
            rewritten.append("Connection: close")
            saw_connection = True
        else:
            rewritten.append(line)
    if not saw_connection:
        rewritten.append("Connection: close")
    return (
        "\r\n".join(rewritten).encode("iso-8859-1")
        + b"\r\n\r\n"
        + request[header_end + 4 :]
    )


def surface_acknowledgement_revision(request: bytes) -> int | None:
    header_end = request.find(b"\r\n\r\n")
    if header_end < 0:
        return None
    request_line = request[:header_end].split(b"\r\n", 1)[0]
    if request_line != b"POST /api/action HTTP/1.1":
        return None
    body = json.loads(request[header_end + 4 :])
    if body.get("action") != "surface-instruction-rendered":
        return None
    revision = body.get("revision")
    if not isinstance(revision, int) or isinstance(revision, bool):
        raise BrowserContractError("surface acknowledgement lacks an integer revision")
    return revision


def response_status(response: bytes) -> int:
    status_line = response.split(b"\r\n", 1)[0]
    parts = status_line.split(b" ", 2)
    if len(parts) < 2 or parts[0] != b"HTTP/1.1":
        raise BrowserContractError("upstream returned an invalid HTTP status line")
    return int(parts[1])


class FixtureProcess:
    def __init__(self, fixture_binary: Path) -> None:
        self.stderr_file = tempfile.TemporaryFile(mode="w+b")
        try:
            self.process = subprocess.Popen(
                [str(fixture_binary)],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=self.stderr_file,
                text=True,
                bufsize=1,
            )
        except BaseException:
            self.stderr_file.close()
            raise
        if self.process.stdout is None or self.process.stdin is None:
            self.process.terminate()
            self._wait_or_kill()
            self.stderr_file.close()
            raise BrowserContractError("fixture pipes were not created")
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.closed = False
        try:
            self.ready = self._read_message()
            require(
                self.ready.get("ok") is True,
                f"fixture failed to start: {self.ready}",
            )
        except BaseException:
            self.process.terminate()
            self._wait_or_kill()
            self._close_handles()
            raise

    def command(self, command: str) -> dict[str, Any]:
        if self.process.stdin is None:
            raise BrowserContractError("fixture stdin is unavailable")
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()
        response = self._read_message()
        require(response.get("ok") is True, f"fixture command failed: {response}")
        return response

    def close(self) -> None:
        if self.closed:
            return
        failure: str | None = None
        try:
            if self.process.poll() is None:
                try:
                    self.command("quit")
                except (BrowserContractError, BrokenPipeError, OSError, ValueError):
                    self.process.terminate()
            self._wait_or_kill()
            if self.process.returncode != 0:
                failure = (
                    f"fixture exited with {self.process.returncode}: "
                    f"{self._stderr_text()}"
                )
        finally:
            self._close_handles()
        if failure is not None:
            raise BrowserContractError(
                failure
            )

    def _read_message(self) -> dict[str, Any]:
        events = self.selector.select(timeout=15)
        if not events:
            raise BrowserContractError(
                f"fixture response timed out: {self._stderr_text()}"
            )
        if self.process.stdout is None:
            raise BrowserContractError("fixture stdout is unavailable")
        line = self.process.stdout.readline()
        if not line:
            raise BrowserContractError(
                f"fixture closed its protocol: {self._stderr_text()}"
            )
        value = json.loads(line)
        if not isinstance(value, dict):
            raise BrowserContractError("fixture response is not a JSON object")
        return value

    def _stderr_text(self) -> str:
        size = os.fstat(self.stderr_file.fileno()).st_size
        offset = max(0, size - 64 * 1024)
        return os.pread(self.stderr_file.fileno(), size - offset, offset).decode(
            errors="replace"
        )

    def _wait_or_kill(self) -> None:
        if self.process.poll() is None:
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)

    def _close_handles(self) -> None:
        if self.closed:
            return
        self.closed = True
        self.selector.close()
        if self.process.stdin is not None:
            self.process.stdin.close()
        if self.process.stdout is not None:
            self.process.stdout.close()
        self.stderr_file.close()


class RealChrome:
    def __init__(self, binary: str, session: str) -> None:
        self.binary = binary
        self.session = session
        self.socket_directory = Path(f"/tmp/ikb-ab-{os.getpid()}")
        self.environment = os.environ.copy()
        self.environment["AGENT_BROWSER_ALLOWED_DOMAINS"] = "127.0.0.1,localhost"
        self.environment["AGENT_BROWSER_DEFAULT_TIMEOUT"] = "15000"
        # Bazel's run directory can exceed the Unix-domain socket path limit.
        # This process-unique loopback-browser directory is short and removed
        # after the named session closes.
        self.environment["AGENT_BROWSER_SOCKET_DIR"] = str(self.socket_directory)

    def open(self, url: str) -> None:
        self.run("open", url)
        self.run(
            "wait",
            "--fn",
            "document.querySelector('#engineering-details').textContent.includes('surface-validate')",
        )

    def evaluate(self, source: str) -> Any:
        encoded = base64.b64encode(source.encode()).decode()
        completed = self.run("eval", "-b", encoded)
        return decode_evaluation(completed.stdout)

    def screenshot(self, path: Path) -> None:
        self.run("screenshot", str(path))

    def close(self) -> None:
        try:
            self.run("close")
        finally:
            shutil.rmtree(self.socket_directory, ignore_errors=True)

    def run(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        completed = subprocess.run(
            [
                self.binary,
                "--session",
                self.session,
                "--engine",
                "chrome",
                *arguments,
            ],
            env=self.environment,
            text=True,
            capture_output=True,
            timeout=30,
            check=False,
        )
        if completed.returncode != 0:
            raise BrowserContractError(
                f"agent-browser {' '.join(arguments)} failed\n"
                f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
            )
        return completed


def decode_evaluation(output: str) -> Any:
    value: Any = output.strip()
    for _ in range(3):
        if not isinstance(value, str):
            break
        try:
            value = json.loads(value)
        except json.JSONDecodeError:
            break
    return value


def required_runfile(setting_name: str) -> Path:
    runfiles_root = os.environ.get("TEST_SRCDIR")
    relative_path = os.environ.get(setting_name)
    if runfiles_root is None or relative_path is None:
        raise BrowserContractError(f"missing Bazel runfile setting: {setting_name}")
    path = Path(runfiles_root) / relative_path
    if not path.is_file():
        raise BrowserContractError(f"missing Bazel runfile: {path}")
    return path


def require(condition: bool, message: str) -> None:
    if not condition:
        raise BrowserContractError(message)


def require_revision(response: dict[str, Any]) -> int:
    revision = response.get("surface_instruction_revision")
    if not isinstance(revision, int) or isinstance(revision, bool):
        raise BrowserContractError(f"fixture did not publish a receipt revision: {response}")
    return revision


def require_receipt(response: dict[str, Any], expected_revision: int) -> int:
    revision = response.get("instruction_revision")
    host_time_ns = response.get("acknowledged_host_time_ns")
    require(
        revision == expected_revision,
        f"receipt revision {revision} does not match {expected_revision}",
    )
    require(
        isinstance(host_time_ns, int)
        and not isinstance(host_time_ns, bool)
        and host_time_ns > 0,
        f"receipt has no positive monotonic host time: {response}",
    )
    return host_time_ns


def attempt_values(evidence: ProxyEvidence) -> list[dict[str, Any]]:
    return [asdict(attempt) for attempt in evidence.snapshot()]


def main() -> int:
    fixture_binary = required_runfile(FIXTURE_RUNFILE_ENV)
    agent_browser = os.environ.get("AGENT_BROWSER_BIN") or shutil.which("agent-browser")
    if agent_browser is None:
        raise BrowserContractError("agent-browser is required for the manual Chrome contract")

    evidence = ProxyEvidence()
    fixture = FixtureProcess(fixture_binary)
    proxy: ReceiptProxyServer | None = None
    proxy_thread: threading.Thread | None = None
    browser: RealChrome | None = None

    try:
        backend_url = fixture.ready.get("backend_url")
        if not isinstance(backend_url, str):
            raise BrowserContractError("fixture did not publish its backend URL")
        parsed_backend = urlsplit(backend_url)
        require(
            parsed_backend.hostname == "127.0.0.1",
            "fixture backend is not loopback",
        )
        require(parsed_backend.port is not None, "fixture backend has no port")
        proxy = ReceiptProxyServer(
            (parsed_backend.hostname, parsed_backend.port), backend_url, evidence
        )
        proxy_thread = threading.Thread(target=proxy.serve_forever, daemon=True)
        proxy_thread.start()
        proxy_url = f"http://127.0.0.1:{proxy.server_address[1]}"
        browser = RealChrome(agent_browser, "r")
        browser.open(proxy_url)
        frozen = browser.evaluate(
            """
            (async () => {
              const deadline = performance.now() + 3000;
              while ((refreshTimer === null || heartbeatInFlight)
                     && performance.now() < deadline) {
                await new Promise((resolve) => window.setTimeout(resolve, 5));
              }
              if (refreshTimer === null || heartbeatInFlight) {
                return JSON.stringify({ frozen: false });
              }
              // No timer or promise continuation can interleave with this JS
              // task. The fixture now owns lease maintenance, and no stale
              // refresh can render the first cue behind the guard checks.
              refreshStopped = true;
              window.clearTimeout(refreshTimer);
              refreshTimer = null;
              heartbeatInFlight = true;
              return JSON.stringify({
                frozen: true,
                phase: JSON.parse(details.textContent).phase,
              });
            })()
            """
        )
        require(
            frozen == {"frozen": True, "phase": "surface-validate"},
            f"could not freeze browser polling at a clean boundary: {frozen}",
        )

        first_revision = require_revision(fixture.command("publish-first"))
        guarded = browser.evaluate(
            """
            (async () => {
              const afterProductionFrames = () => new Promise((resolve) => {
                window.requestAnimationFrame(() => {
                  window.requestAnimationFrame(() => window.queueMicrotask(resolve));
                });
              });
              const state = await api('/api/state');
              const resultState = { ...state, screen: 'validation-result' };
              render(resultState);
              await afterProductionFrames();
              const resultScreen = {
                inFlight: surfaceInstructionAcknowledgementInFlightRevision,
                resultVisible: !document.querySelector('#screen-validation-result').hidden,
                surfaceHidden: surfaceTransferScreen.hidden,
              };

              render(state);
              surfaceTransferScreen.hidden = true;
              await afterProductionFrames();
              const hiddenSurface = {
                inFlight: surfaceInstructionAcknowledgementInFlightRevision,
                surfaceHidden: surfaceTransferScreen.hidden,
              };

              render(state);
              surfacePrompt.textContent = 'Not the pending cue';
              await afterProductionFrames();
              const mismatchedText = {
                inFlight: surfaceInstructionAcknowledgementInFlightRevision,
                expected: state.surface_target,
                actual: surfacePrompt.textContent,
              };
              return JSON.stringify({ resultScreen, hiddenSurface, mismatchedText });
            })()
            """
        )
        require(isinstance(guarded, dict), f"invalid browser guard result: {guarded}")
        require(
            guarded["resultScreen"] == {
                "inFlight": None,
                "resultVisible": True,
                "surfaceHidden": True,
            },
            f"result-screen guard failed: {guarded}",
        )
        require(
            guarded["hiddenSurface"] == {"inFlight": None, "surfaceHidden": True},
            f"hidden-surface guard failed: {guarded}",
        )
        require(
            guarded["mismatchedText"]["inFlight"] is None,
            f"mismatched cue started a receipt: {guarded}",
        )
        require(evidence.snapshot() == [], "a non-presented cue reached the service")

        visible = browser.evaluate(
            """
            (async () => {
              const afterProductionFrames = () => new Promise((resolve) => {
                window.requestAnimationFrame(() => {
                  window.requestAnimationFrame(() => window.queueMicrotask(resolve));
                });
              });
              render(await api('/api/state'));
              await afterProductionFrames();
              return JSON.stringify({
                inFlight: surfaceInstructionAcknowledgementInFlightRevision,
                pending: pendingSurfaceInstructionRevision,
                target: surfacePrompt.textContent,
                surfaceHidden: surfaceTransferScreen.hidden,
              });
            })()
            """
        )
        evidence.wait_for_first_accepted()
        require(visible["inFlight"] == first_revision, f"exact cue was not in flight: {visible}")
        require(visible["pending"] == first_revision, f"exact cue lost its pending state: {visible}")
        require(visible["surfaceHidden"] is False, f"exact cue was hidden: {visible}")
        first_receipt = fixture.command(f"await-take {first_revision}")
        first_receipt_time_ns = require_receipt(first_receipt, first_revision)

        replacement = fixture.command("publish-replacement")
        require(
            replacement.get("surface_instruction_revision") is None,
            f"non-receipt replacement retained a revision: {replacement}",
        )
        cleared = browser.evaluate(
            """
            (async () => {
              const state = await api('/api/state');
              render(state);
              return JSON.stringify({
                pending: pendingSurfaceInstructionRevision,
                inFlight: surfaceInstructionAcknowledgementInFlightRevision,
                target: surfacePrompt.textContent,
                serviceRevision: state.surface_instruction_revision,
              });
            })()
            """
        )
        require(cleared["pending"] is None, f"service null was not authoritative: {cleared}")
        require(cleared["inFlight"] == first_revision, f"old fetch settled too early: {cleared}")
        require(cleared["serviceRevision"] is None, f"replacement is receipt-bound: {cleared}")

        stale = browser.evaluate(
            f"""
            (async () => {{
              try {{
                await postAction('surface-instruction-rendered', {first_revision});
                return JSON.stringify({{ rejected: false }});
              }} catch (error) {{
                return JSON.stringify({{ rejected: true, message: error.message }});
              }}
            }})()
            """
        )
        require(stale == {"rejected": True, "message": "Local service returned 400"}, f"old revision was not rejected: {stale}")

        second_revision = require_revision(fixture.command("publish-second"))
        queued_second = browser.evaluate(
            """
            (async () => {
              const afterProductionFrames = () => new Promise((resolve) => {
                window.requestAnimationFrame(() => {
                  window.requestAnimationFrame(() => window.queueMicrotask(resolve));
                });
              });
              const state = await api('/api/state');
              render(state);
              await afterProductionFrames();
              return JSON.stringify({
                pending: pendingSurfaceInstructionRevision,
                inFlight: surfaceInstructionAcknowledgementInFlightRevision,
                target: surfacePrompt.textContent,
              });
            })()
            """
        )
        require(queued_second["pending"] == second_revision, f"new cue was not pending: {queued_second}")
        require(queued_second["inFlight"] == first_revision, f"old fetch identity was lost: {queued_second}")
        require(len(evidence.snapshot()) == 2, "new cue bypassed the older in-flight request")

        evidence.release()
        recovered = browser.evaluate(
            f"""
            (async () => {{
              const deadline = performance.now() + 3000;
              const nextFrame = () => new Promise((resolve) => window.requestAnimationFrame(resolve));
              while (surfaceInstructionAcknowledgementInFlightRevision !== null
                     && performance.now() < deadline) {{
                await nextFrame();
              }}
              render(await api('/api/state'));
              while (lastAcknowledgedSurfaceInstructionRevision !== {second_revision}
                     && performance.now() < deadline) {{
                await nextFrame();
              }}
              return JSON.stringify({{
                pending: pendingSurfaceInstructionRevision,
                inFlight: surfaceInstructionAcknowledgementInFlightRevision,
                acknowledged: lastAcknowledgedSurfaceInstructionRevision,
              }});
            }})()
            """
        )
        require(
            recovered == {
                "pending": None,
                "inFlight": None,
                "acknowledged": second_revision,
            },
            f"new cue did not recover after response loss: {recovered}",
        )
        second_receipt = fixture.command(f"await-take {second_revision}")
        second_receipt_time_ns = require_receipt(second_receipt, second_revision)
        require(
            second_receipt_time_ns > first_receipt_time_ns,
            "the second browser receipt did not follow the first on the shared host clock",
        )
        attempts = attempt_values(evidence)
        require(
            attempts == [
                {
                    "revision": first_revision,
                    "status": 200,
                    "disposition": "held-then-dropped",
                },
                {
                    "revision": first_revision,
                    "status": 400,
                    "disposition": "forwarded",
                },
                {
                    "revision": second_revision,
                    "status": 200,
                    "disposition": "forwarded",
                },
            ],
            f"unexpected acknowledgement sequence: {attempts}",
        )

        final_fixture_state = fixture.command("status")
        require(
            final_fixture_state.get("phase") == "surface-validate"
            and final_fixture_state.get("screen") == "surface-transfer"
            and final_fixture_state.get("surface_instruction_revision") is None
            and final_fixture_state.get("service_error") is None
            and final_fixture_state.get("paused") is False,
            f"fixture did not finish healthy with no pending receipt: {final_fixture_state}",
        )
        engine_probe = browser.evaluate(
            "JSON.stringify({ userAgent: navigator.userAgent, chromeObject: typeof window.chrome === 'object' })"
        )
        require(
            isinstance(engine_probe, dict)
            and "Chrome/" in engine_probe.get("userAgent", "")
            and engine_probe.get("chromeObject") is True
            and "Lightpanda" not in engine_probe.get("userAgent", ""),
            f"agent-browser did not drive Chrome: {engine_probe}",
        )
        agent_browser_version = subprocess.run(
            [agent_browser, "--version"],
            text=True,
            capture_output=True,
            timeout=10,
            check=True,
        ).stdout.strip()
        receipt = {
            "schema": "invisible-keyboard-synthetic-hardware-browser-receipt-contract-v1",
            "claim_scope": (
                "Real Chrome executes production calibration-app.js against real CalibrationApp "
                "HTTP endpoints through an IPv4-loopback TCP fault proxy and synthetic "
                "hardware-mode state. CDP injects result-screen, hidden-element, and mismatched-text "
                "branches. The proxy rewrites Origin to the backend origin before forwarding and "
                "drops one accepted response. This does not test direct Origin rejection, prove "
                "pixel paint, visibility, or lack of occlusion, or establish sensor contact, "
                "latency, accuracy, or physical feasibility."
            ),
            "application_network_scope": (
                "The app service and fault proxy bind 127.0.0.1, and browser page/subresource "
                "navigation is allowlisted to 127.0.0.1 and localhost. This field does not claim "
                "observation of tool telemetry or operating-system traffic."
            ),
            "branch_control": "CDP-injected synthetic state and DOM mutation",
            "origin_boundary": "proxy rewrites Origin; direct service Origin enforcement is outside this receipt",
            "browser_engine": "chrome",
            "agent_browser_version": agent_browser_version,
            "browser_probe": engine_probe,
            "first_receipt": first_receipt,
            "second_receipt": second_receipt,
            "guard_observations": guarded,
            "response_loss_clear": cleared,
            "new_cue_recovery": recovered,
            "acknowledgement_attempts": attempts,
            "final_fixture_state": final_fixture_state,
        }
        formatted_receipt = json.dumps(receipt, sort_keys=True, indent=2)
        undeclared_outputs = os.environ.get("TEST_UNDECLARED_OUTPUTS_DIR")
        if undeclared_outputs is not None:
            output_directory = Path(undeclared_outputs)
            output_directory.mkdir(parents=True, exist_ok=True)
            browser.screenshot(output_directory / "calibration-browser-receipt.png")
            (output_directory / "calibration-browser-receipt.json").write_text(
                formatted_receipt + "\n", encoding="utf-8"
            )
        print(formatted_receipt)
        return 0
    finally:
        active_error = sys.exc_info()[1]
        cleanup_errors: list[str] = []
        evidence.release()
        if browser is not None:
            try:
                # Suppress production lifecycle actions during test teardown.
                # Closing the fixture first must not look like lease loss.
                browser.evaluate(
                    "captureLeaseRevision = null; refreshStopped = true; heartbeatInFlight = true; true"
                )
            except (BrowserContractError, OSError, subprocess.SubprocessError) as error:
                cleanup_errors.append(f"browser neutralization: {error}")
        try:
            fixture.close()
        except (BrowserContractError, OSError, ValueError) as error:
            cleanup_errors.append(f"fixture close: {error}")
        if browser is not None:
            try:
                browser.close()
            except (BrowserContractError, OSError, subprocess.SubprocessError) as error:
                cleanup_errors.append(f"browser close: {error}")
        if proxy is not None:
            try:
                if proxy_thread is not None and proxy_thread.is_alive():
                    proxy.shutdown()
                proxy.server_close()
                if proxy_thread is not None:
                    proxy_thread.join(timeout=5)
                    if proxy_thread.is_alive():
                        raise BrowserContractError("proxy thread did not stop")
            except (BrowserContractError, OSError) as error:
                cleanup_errors.append(f"proxy close: {error}")
        if cleanup_errors:
            message = "; ".join(cleanup_errors)
            if active_error is None:
                raise BrowserContractError(message)
            print(f"additional cleanup failures: {message}", file=sys.stderr)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (
        BrowserContractError,
        OSError,
        ValueError,
        json.JSONDecodeError,
        subprocess.SubprocessError,
    ) as error:
        print(f"calibration browser receipt contract failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
