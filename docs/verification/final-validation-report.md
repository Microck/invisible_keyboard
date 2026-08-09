# Final software validation

Date: 2026-10-13. Frozen source: v13.

## Status and scope

The hardware-independent deliverable is complete. Frozen software verification
passed, and two consecutive independent completion audits found no material
source or evidence defects. Only the physical gates listed below remain.

The deliverable is the hardware-independent reference stack and executable
[first-build roadmap](../implementation/first-prototype-roadmap.md), not a
working physical keyboard. It preserves Core-28 output, the 34-position physical
grid, the no-language-model decoder, independently written application code,
and the Bazel build. No physical controller, IMU, analyzer, or wearable was
attached or flashed.

This continuation resumes Codex thread
`019fe3d0-c436-73d2-ab82-7de8d17eaa65`. Its acceptance criteria are three
consecutive durability passes on final frozen source; the full nonmanual and
required-capability checks; actual CLI exercise and both pinned firmware
builds; source-bound receipts and artifacts; and two consecutive independent
clean completion audits leaving only irreducible physical gates.

The September receipts in `final-validation/` are historical. They are not
counted as proof of this source.

## Source identity and sandbox boundary

The manifest covers 146 authored source, test, build, lockfile, configuration,
and documentation files. Its SHA-256 is:

```text
5b6c2109e0a521d3470f895b87f8d4b9667e8a3c72da3fdd510ab234ffa478e6
```

The [native before](continuation-validation/source-before.sha256),
[native after](continuation-validation/source-after.sha256),
[guest before](continuation-validation/source-x86-before.sha256), and
[guest after](continuation-validation/source-x86-after.sha256) manifests match
byte for byte. The collector enforced that equality inside Crabbox. A separate
[final host-source comparison](continuation-validation/final-host-source-binding.json)
then matched a fresh archive of the authoritative worktree against both the
tested tree and the frozen manifest at 03:10:57 UTC.

The manifest excludes VCS metadata, the external research corpus, Bazel
outputs, Python bytecode caches, this report, and generated proof directories.
At the end of validation, no commit had been created; the manifest identifies
the source frozen before the version-control handoff.

All accepted builds, tests, lint, sanitizer runs, runtime scenarios, hashes,
and source comparisons ran inside owned Crabbox `cbx_42955e8aa2c5`. It used an
external Docker provider, not GitHub Codespaces.

| Execution surface | Recorded environment | Role |
| --- | --- | --- |
| Native ARM64 container | Ubuntu 22.04.5, Linux 6.8.0-1047-oracle, Python 3.10.12, GCC 11.4 | Full graph, durability, CLI, browser, reducer, sanitizers, capture process contracts |
| Nested x86-64 QEMU guest | Ubuntu 24.04.5, Linux 6.8.0-142-generic, Python 3.12.3, GCC 13.3 | Real nonroot uinput, PTY/ptrace, pinned firmware builds and capture binary |

The complete [native environment](continuation-validation/environment-native.json)
and [guest environment](continuation-validation/environment-x86.json) are
retained. The guest ran as UID 1000 in `invisible-keyboard`; `/dev/uinput` was a
real character device owned by `root:invisible-keyboard`, mode `0660`.

One reviewer ran a read-only host `diff`/`wc` on copied manifests despite the
sandbox-only instruction. That comparison is excluded from accepted proof.
The reviewer withdrew it; the authoritative comparisons above ran in Crabbox.

## Frozen verification results

The [inventory](continuation-validation/inventory.json) records all 13 successful
lanes, exact commands, UTC intervals, exit codes, 53 JUnit target receipts, and
available inner test counts. All Bazel test results were uncached. Build actions
could reuse pinned caches. No accepted JUnit receipt contains a failure, error,
or skipped case.

Bazel's JUnit wrapper usually records one case per target; 53 target receipts
must not be read as 53 individual tests. The nonmanual trace suite intentionally
ignores its full-size case, which ran separately below.

| Check | Result | Receipt |
| --- | --- | --- |
| Full nonmanual graph | 40/40 targets, including Clippy, Rustfmt and native C conformance | [Log](continuation-validation/native/nonmanual.log) |
| Syscall durability | Three consecutive uncached passes on 2026-10-11, 10:47:05–10:49:56 UTC | [Binding](continuation-validation/native/durability-binding.json) |
| Actual application commands | CLI scenarios passed, including expected rejection paths | [Log](continuation-validation/native/application-smoke.log) |
| Real browser | Chrome receipt, retry and surface-guard exercise passed | [Log](continuation-validation/native/browser.log) |
| Full raw reducer | 14,440,000,000 bytes; one passed, zero failed/ignored, 38 filtered; 1307.40 seconds | [Log](continuation-validation/native/full-size-reducer.log) |
| Native C sanitizers | All four targets passed with ASan and UBSan; no runtime-error diagnostics | [Log](continuation-validation/native/native-sanitizers.log) |
| Required nonroot Linux output | Six passed, zero failed/ignored/filtered; 28.04 seconds | [Log](continuation-validation/x86/required-output.log) |
| Real PTY and post-footer ptrace | Nine passed, zero failed/ignored; 23.74 seconds | [Log](continuation-validation/x86/x86-tty-and-footer.log) |
| Selected-board firmware | Left and right ELF, HEX and UF2 files plus provenance built | [Log](continuation-validation/x86/selected-board-firmware.log) |
| Capture stack | All 20 cases: 18 native process/finalizer cases and two pinned x86 binary cases | [Coverage](continuation-validation/capture-test-coverage.json) |

The durability binding checks each archived raw strace against its receipt and
binds all three runs to the same CLI and CLI-test executable. No durability
assertion or application deadline was relaxed.

The required-output run exercised 10,000 tap pairs, every Core-28 key,
held-key admission, owned-device destruction on SIGKILL, and 20 planned
SIGINT/SIGTERM cycles. Both 250 ms planned-shutdown assertions remain. The
fixture sends the signal directly rather than including an external `kill`
program's startup in that measurement. SIGKILL device destruction does not
establish a 250 ms all-up guarantee.

The full-size reducer fixture deliberately contains a stale hash and expects
rejection after the complete scan. It proves the bounded-streaming processing
path, not a successful physical analyzer trace. The four sanitizer targets
execute portable C policies natively; the firmware build does not execute those
cases on a controller.

### Capture execution and interrupted-run recovery

Full-system x86 emulation exceeded the capture supervisor's unchanged 10 ms
polling limit. That failed run remains in
[the regression evidence](continuation-validation/regressions/capture-before-native-partition-v13/capture-stack.log).
The timing-sensitive Python classes then ran natively in the same Crabbox,
using the same pinned analyzer firmware bundle. The pinned x86 executable's
identity, provenance, dependencies and loadability ran in the guest.

The collector discovered the suite from frozen source and asserted that the
two passing case sets have no overlap and their union is exactly all 20 cases.
No test, assertion, application deadline, or production source changed for this
partition. Missing-input and resume-path mistakes in the proof orchestration
are retained separately and are not passing evidence.

The native run retained its successful six-lane prefix across an interruption,
then completed the reducer and sanitizers on October 13. The reducer was rerun
because its earlier passing test lacked completed harness metadata. The guest
retained its three successful lanes when moving capture process checks to the
native surface. Runners checked source identity and exact successful-prefix
commands before reuse, then checked source identity again after completion.
The final collector reconciled both platforms. Interrupted and failed runs are
not included in the passing counts.

## Actual CLI and browser behaviour

The [application smoke](continuation-validation/native/application-smoke.log)
exercised the built CLI, not only unit-test functions:

- A progressing HTTP request received 408 after about 0.118 seconds; the next
  request received 200.
- `score-pilot` returned `stop`, non-armable, and `hardware_truth=not-asserted`
  for the synthetic insufficient-contact fixture.
- Physical-study plan, seal and score commands rejected clipped lower and
  upper timestamp intervals. Complete boundary intervals scored normally;
  their synthetic `go` or `no-go` decisions are not human-study results.
- Calibrated replay at 999 ns/µs processed 202 frames without faults and
  emitted the exact mapped A/H transitions, ending neutral. At 1001 ns/µs,
  repeated raw sensor time was rejected at record 2 with no output events.
  Both input bundles remained unchanged.

Healthy/current-reset and busy-watchdog live CLI checks also passed during the
preceding targeted repair verification; those retained regression receipts are
separate from the frozen application-smoke lane.

The [browser archive](continuation-validation/native/browser-outputs.zip)
contains the JSON receipt and screenshot. Chrome 156 and agent-browser 0.36.0
exercised lost-response retry, stale receipt rejection, a later accepted cue,
and hidden, mismatched-text and result-screen guards. CDP-injected branches
remain synthetic. The fault proxy rewrites Origin, so this browser run does not
prove direct Origin rejection. It also does not prove pixel paint, occlusion,
physical cue perception, or sensor truth.

## Retained artifacts and reproducibility

The [artifact bundle](continuation-validation/artifacts/validated-x86-firmware-capture-and-source.tar.gz)
contains the x86-64 CLI, both firmware variants and provenance, the pinned
capture stack, its corresponding-source archives, license texts, build logs,
checksums, and the frozen reference-source archive. The native
[ARM64 CLI](continuation-validation/artifacts/invisible-keyboard-aarch64) is
retained separately.

Bundle SHA-256:
`67a50e4e27cf88448b31385866057d4187a597ed6a76461bcec537d93688421d`.

| Artifact | SHA-256 |
| --- | --- |
| ARM64 CLI | `9b57ff02a07680c4a758fdec4de6afb62c9a1b31294f91cef462bd14f4440518` |
| x86-64 CLI | `fddd7d22fb033981dc147e9ce9045870bc30fd55317fd5d0cce1c82001d40878` |
| Left UF2 | `8c31c4e9fdeec457db4548587d3f00421803164d884de703d6503544d692134a` |
| Right UF2 | `4786fe648e31e281191fe669434480b3495f223334d3aa2d90264886246e5831` |
| Pinned `sigrok-cli-gate0a` | `1665066921d1163bb2dddd45ae2ea20b55cf6970ac8de46ef5b0dbd2804e65c3` |

The capture build reproduced the existing binary, BUILD-INFO, firmware-bundle,
patch and provenance pins. No alternate pin was accepted. Bazel 9.2.0,
rules_rust 0.73.0, Rust 1.94.0 and the Zephyr 4.4.0 recipe remain pinned.
The guest build emitted a gold-linker deprecation warning; it did not fail.

The [receipt manifest](continuation-validation/receipt-hashes.sha256) binds
retained evidence. Runner scripts and execution metadata are included for
reproduction. At the end of validation, artifacts remained local. Nothing had
been uploaded, committed, pushed, released, deployed, or flashed.

## Completion audits

The two independent reviewers traced the whole deliverable: firmware and host
quiesce; capture and finalization; frame and clock admission; calibration,
keyboard labels and artifact authority; live/replay output and watchdogs;
session durability and incidents; browser control; pilot and physical-study
admission; and assembly/roadmap handoffs.

[CompletionAuditT](continuation-validation/audit-t.md) and
[CompletionAuditS](continuation-validation/audit-s.md) issued independent clean
completion verdicts on the same v13 source. No source changes intervened.
Both reviewed the completed native and guest proof; the final package check
also closed the remaining archive-inspection caveat. Digests and source
equality were computed in Crabbox, not recomputed by the reviewers.

Crabbox `cbx_42955e8aa2c5` was released after evidence sealing and local
collection. The provider removed its container, including the nested VM.
`crabbox list --all --json` then returned `[]`, confirming cleanup.

## Physical-only gates

Software verification cannot close these gates:

1. **Received parts and assembly:** qualify the exact board, harness, fixed
   total-input envelope, thermal behaviour and wear admission (Gates A, B and
   E). A regulator rating is not a board-level guarantee. Keep `PWR_LINK` open
   until the assembly contract permits connection.
2. **Bring-up and Gate 0A:** flash/read back both hands; verify actual sensor
   registers, interrupt edges, I2C, USB, D8/D9, quiesce, idle-tail timing and the
   sustained analyzer soak.
3. **Commissioning:** collect real six-face axis poses and the finite
   nine-attempt human pilot.
4. **Observability and transfer:** establish contact and acting-finger
   observability, held-out separability of all 34 positions with Core-28
   output, remount/surface/person transfer, and false activation during
   ordinary motion (Gates 0B, 1, 2 and 6–8).
5. **Host and use:** test real focus, IME, suspend and disconnect (Gate 0S),
   then live text, speed, error rate, comfort and safety (Gates 3–5 and 9).

The [assembly baseline](../implementation/rev-a-assembly-baseline.md),
[physical study protocol](../implementation/physical-study-protocol.md), and
[first-build roadmap](../implementation/first-prototype-roadmap.md) define the
procedures and acceptance decisions. Passing synthetic fixtures in a real
browser or kernel does not establish physical feasibility.
