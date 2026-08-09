# Hardware-independent bug bash

This report records the adversarial passes against the Bazel-built CLI,
session/evidence pipeline, firmware contracts, Gate 0A scorer, Linux output,
hardware-runner contracts, and local calibration service. Each finding was
reproduced or established from an executable timing or evidence contradiction
before its fix. Findings 001 through 029 retain their dated regression record.
Findings 030 through 053 record the later hard cuts and their final test names.
The physical runner still needs the two real controllers, twelve sensors,
analyzer, and a keyboard before its device continuity can be exercised end to
end.

## Run identity

- Target: the Bazel-built CLI, firmware contracts, analyzer scorer, Linux
  output adapter, and loopback HTTP service
- Original pass: 2026-08-12 UTC on Linux 6.8.0-1058-oracle, AArch64
- Follow-up closure pass: 2026-08-29 UTC on Crabbox `bx_v97q4knd`
- Rust: 1.94.0
- Bazel: 9.2.0
- Evidence class: CLI, HTTP, portable/native firmware, simulated, replay,
  native raw-analyzer reduction, and native Linux `uinput` tests
- Physical hardware: not used

## Summary

| Severity | Found | Open |
| --- | ---: | ---: |
| Critical | 2 | 0 |
| High | 42 | 0 |
| Medium | 8 | 0 |
| Low | 1 | 0 |

These counts include Findings 001 through 053. `Open` counts
hardware-independent defects in the settled source. It does not count the
physical Gate 0A, contact-observability, separability, wear, or
human-performance gates.

The final frozen-source result belongs in the
[final validation report](final-validation-report.md). Consult its status and
exact source manifest; the dated finding-level runs below do not replace it.

The pass also checked normal and invalid command dispatch, manifest parsing,
exclusive file creation, session truncation, malformed COBS retention, clock
fit refusal, CSRF controls, request size limits, pause/resume behavior, all
seven setup screens, and the finite `validation-passed` transition. No additional defect was
reproduced in those areas.

## Surface area exercised

The original pass used this historical orientation command:

```sh
./bazel-bin/invisible-keyboard --help
```

The pass exercised these command families:

- `layout`, `simulate`, and `frame-fixture`
- `session-fixture`, `record-stream`, and `verify-session`
- `fit-session-clocks`
- calibration evidence, fit, validation, and complete replay contracts
- `calibration-demo` HTTP assets, state, and actions
- portable C and selected Zephyr firmware conformance
- Gate 0A native raw `u16` analyzer reduction and scorecard contracts
- optional and required native Linux `uinput` lanes
- invalid commands, operands, files, ports, JSON, methods, origins, tokens,
  request sizes, incomplete requests, and existing output paths

## Finding 001: malformed HTTP request terminated calibration

- Severity: High
- Status: Resolved
- Area: loopback calibration service availability

Before the fix, one malformed connection returned no HTTP response and caused
`serve_until` to exit. The listener disappeared, so a browser could no longer
view or stop the current setup.

Reproduction:

```sh
./bazel-bin/invisible-keyboard calibration-demo 3043
/usr/bin/printf 'BROKEN\r\n\r\n' | nc 127.0.0.1 3043
```

Observed process error:

```text
error: calibration failed: malformed HTTP request: path is missing
```

Expected behavior: reject only that request and continue serving.

Fix: `CalibrationHttpServer::handle` now turns parser failures into an HTTP 400
response. The accept loop stays alive.

Crabbox regression evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:calibration-app-contract-test
```

The permanent test is `malformed_http_isolated_to_one_connection`.

## Finding 002: idle connection starved every later request

- Severity: High
- Status: Resolved
- Area: loopback calibration service availability

Before the fix, a client could connect and send no complete header. The single
service thread waited forever in `read`, and a valid request behind it timed
out.

Reproduction:

```sh
nc 127.0.0.1 3044
curl -sS --max-time 1 -i http://127.0.0.1:3044/
```

Observed client error:

```text
curl: (28) Operation timed out after 1001 milliseconds with 0 bytes received
```

Expected behavior: bound the bad connection and resume normal service.

Fix at this stage: accepted streams gained two-second read and write deadlines.
An idle request received HTTP 408, and connection-local I/O failures no longer
stopped the calibration process. Finding 024 later tightened the current
deadline to 100 ms so the bounded wait also fits the controller channel budget.

Crabbox regression evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:calibration-app-contract-test
```

The permanent test is
`stalled_http_connection_times_out_without_blocking_the_service`.

## Finding 003: undeclared controller hand entered verified evidence

- Severity: High
- Status: Resolved
- Area: acquisition provenance and clock fitting

A manifest declaring only the left controller could be paired with a session
containing left and right frames. `verify-session` reported success, and clock
fitting silently ignored all right-hand frames.

Observed output before the fix:

```text
evidence=replay-tested
left_received=32
right_received=32
```

The same one-hand manifest also allowed the two-hand `session-fixture` command
to create the contradiction.

Expected behavior: every decoded hand must be declared by the exact manifest.
Fitting and held-out validation must also observe every declared hand.

Fix: `AcquisitionManifest` now owns the observed-topology checks. Verification,
clock fitting, capture alignment, and calibration binding reject undeclared
hands. Strong fitting gates also reject missing declared hands. The fixed
two-hand fixture refuses a one-hand manifest before it creates a file.

Crabbox regression evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:manifest-contract-test //:clock-epoch-contract-test //:cli-contract-test
```

The fixed CLI now reports:

```text
session contains decoded frames for undeclared Right hand
```

## Finding 004: replayed rejected-example count was lost

- Severity: Medium
- Status: Resolved
- Area: calibration state truthfulness

The fitting evidence sidecar stored rejected examples, but the complete
evidence-driven setup replay never applied that count to the coordinator. The
final UI snapshot therefore showed zero rejected examples for evidence that
contained rejections.

Expected behavior: UI state must match the immutable evidence without looping
once per rejected event or changing accepted coverage.

Fix: the coordinator accepts one bounded aggregate rejection count, and the
evidence replay applies it during collection. A contract assertion compares
the final snapshot with `fitting_evidence.rejected_examples`.

Crabbox regression evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:calibration-engine-contract-test //:calibration-contract-test
```

## Finding 005: overlapping polls could render stale setup state

- Severity: Medium
- Status: Resolved
- Area: calibration browser state

The page started a new fetch every second even if the previous request was
still pending. An older state response could arrive after an action response
and replace the newer screen, progress, or pause state.

Expected behavior: state must move forward monotonically in the browser.

Fix: each application mutation increments a serialized revision. The browser
uses a single-flight timeout loop, skips older revisions, and avoids redundant
full renders for an unchanged revision.

Crabbox regression evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:calibration-app-contract-test //:rustfmt-test //:clippy-test
```

## Finding 006: missing input was mislabeled as output failure

- Severity: Low
- Status: Resolved
- Area: CLI error copy

Reproduction:

```sh
./bazel-bin/invisible-keyboard fit-session-clocks \
  /no/such/manifest /no/such/session /tmp/no-output
```

The command previously printed `output failed` even though opening the first
input path failed. The common message now says `file I/O failed`, which is true
for both reads and writes.

Crabbox regression evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:cli-contract-test
```

## Finding 007: the stored surface target did not prove what the user saw

- Severity: High
- Status: Resolved
- Area: passive-surface label timing

The physical runner scheduled a surface prompt and used its hidden planned
time as the target timestamp. The browser did not report when the cue became
visible. A tap could therefore receive a label relative to a server timer that
did not match the instruction the user had actually seen.

Expected behavior: no prompted surface response gate can open until the
visible, focused browser acknowledges the exact rendered instruction. The
acknowledgment proves presentation only; it cannot prove contact.

Fix: every target now carries a monotonic instruction revision. The browser
waits for two animation frames after rendering, then acknowledges that exact
revision. The server consumes the receipt once, shifts the remaining schedule
by measured render delay, opens the bounded response gate, and stores planned,
published, and acknowledged host times in the strict capture journal. Focus or
lease loss aborts the attempt. Finding 027 covers the separate causal
sensor-contact requirement.

Crabbox regression evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:calibration-app-contract-test //:calibration-capture-contract-test
```

The permanent tests are
`a_surface_label_requires_the_browser_to_acknowledge_the_rendered_instruction`
and `passive_surface_receipts_require_one_monotonic_rendered_revision`.

## Finding 008: fitting could expire the browser lease and stop capture checks

- Severity: High
- Status: Resolved
- Area: physical calibration orchestration

Artifact fitting and held-out evaluation ran synchronously in the capture
loop. Either computation can take longer than the five-second browser lease.
During that time, the service could not process heartbeats and the runner did
not drain or validate both controller streams.

Expected behavior: expensive local computation must not suspend browser lease
handling or source-health checks.

Fix: fitting and final validation now run in finite background workers. The
foreground loop keeps serving the local UI and drains both controller streams
through a discard-only validation worker. Those gap frames cannot enter fitting
or held-out evidence. A lease, identity, transport, or controller fault still
terminates the attempt.

Crabbox verification evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:unit-tests //:calibration-app-contract-test //:calibration-worker-contract-test
```

The typed `ArtifactFitting` and `MountValidating` states compile under the full
Bazel graph. Their actual device timing remains part of the first physical run.

## Finding 009: a validation report did not bind its evidence sidecars

- Severity: High
- Status: Resolved
- Area: held-out result provenance

The validation report bound session hashes but not the calibration-evidence
hashes that selected and labeled its scoring windows. Two evidence sidecars
could therefore be swapped without changing the report's declared session
identity.

Historical expected behavior: each held-out session had exactly one immutable
evidence sidecar, and the report proved that pairing.

Fix: calibration evidence now fingerprints its strict capture journal.
Validation report schema v2 stores one evidence hash for each session hash,
rejects missing or duplicate evidence, and includes those hashes in its own
fingerprint.

Crabbox regression evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:calibration-engine-contract-test //:validation-contract-test
```

The permanent test is
`every_validation_session_has_one_bound_evidence_sidecar`.

Current hard cut: this was a valid integrity repair for the earlier API, but an
evidence sidecar and validation report are now derived diagnostic outputs only.
The canonical held-out inputs are the artifact-bound plan, schema-v9 facts
journal, clock fits, acquisition manifest, and raw session. Runtime rederives
the report from those inputs. The schema-v4 fitted artifact still fingerprints
both its fitting journal and derived fitting evidence for provenance; that does
not make the derived file an authority input.

## Finding 010: any nonzero ordinary-motion sample could satisfy validation

- Severity: High
- Status: Resolved
- Area: false-activation exposure

The early held-out contract required ordinary movement but did not enforce a
minimum derived duration. A caller-supplied positive duration could appear to
satisfy this held-out gate without useful negative exposure.

Expected behavior: held-out validation uses retained causal frames, requires
both hands, and counts only measured motion-rich, non-contact, non-saturated
windows toward a fixed minimum independent of sidecar claims.

Fix: the evaluator derives merged retained-frame intervals and requires at
least 60,000 ms of qualified ordinary movement per hand. The immutable held-out
plan schedules three 22-second movement intervals per hand so complete windows
can cover the gate. Quiet null, contact-like, saturated, and caller-supplied
duration do not count.

Crabbox regression evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:calibration-engine-contract-test //:prompt-contract-test //:validation-contract-test
```

The regressions include a 59,999 ms validation failure, forged duration claims,
one-hand-only motion, quiet-null substitution, and explicit motion-segment
qualification.

## Finding 011: the replay demo named a target without highlighting it

- Severity: Medium
- Status: Resolved
- Area: guided calibration presentation

The replay UI displayed `Tap Q` while every cell in its 34-position map looked
inactive. The hardware event path carried a position, but the deterministic
replay derived only the prompt text.

Expected behavior: the text cue and the active map cell name the same physical
position in every executable setup mode.

Fix: the simulated replay derives one display-only position from the same
coordinator state that produces its prompt. It does not alter captured evidence
or the hardware receipt path.

Crabbox regression target and historical browser evidence:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:calibration-app-contract-test
# Historical browser inspection command from the original pass.
agent-browser open http://127.0.0.1:3002
```

The regression checks `Tap Q` against active position `Q`. A live Chromium pass
confirmed one `aria-current=true` Q grid cell, all 34 accessible positions, all
seven screens, and the final `simulated-replay` evidence warning.

## Finding 012: cached validation could arm output without replaying held-out evidence

- Severity: High
- Status: Resolved
- Area: runtime calibration admission

The live and calibrated-replay commands could trust a stored validation report
after checking its artifact binding. A forged, stale, failed, overlapping, or
non-neutral report could therefore cross the runtime trust boundary without
replaying the canonical held-out capture inputs that were supposed to justify
it.

Expected behavior: a report is an untrusted cache. Runtime admission must
recompute it from the immutable held-out bundle and must do so before opening a
controller or creating a `uinput` device.

Current fix: the runtime held-out loader reads the artifact-bound held-out plan,
schema-v9 facts journal, clocks, raw session, and report cache. It
audits complete plan coverage and the raw session, rederives evidence, reruns
validation, compares the complete recomputed report with the cached report, and
creates `HeldOutValidation` only from that result.
`run-calibrated` performs this work before controller `File::open` calls or
output construction. `calibrate-replay` emits the same canonical bundle.
This result authorizes the saved model and replay only. Finding 029 covers the
separate fresh per-process wear-session attestation required for live output.

Permanent regressions:

- `calibrated_replay_arms_only_after_recomputing_the_canonical_heldout_bundle`
- `runtime_admission_rejects_unreplayed_or_failed_evidence_before_device_open`
- `runtime_admission_rejects_forged_and_non_neutral_reports_in_live_and_replay`

Crabbox evidence: `//:cli-contract-test`, `//:rustfmt-test`, and
`//:clippy-test` passed on `bx_v97q4knd` for this change.

## Finding 013: keyboard identity silently stood in for US-QWERTY confirmation

- Severity: High
- Status: Resolved
- Area: calibration layout provenance

Selecting a Linux keyboard proved which event source supplied labels, but it
did not prove that the displayed physical rows matched US QWERTY. A generic app
revision also could not identify which layout instruction the user accepted,
and replay risked replacing source provenance with its current UI revision.

Expected behavior: identification and logical-layout confirmation are distinct
events. Capture stays disarmed until one explicit yes/no decision binds the
dedicated instruction revision and first accepted host time. Replay preserves
the original receipt exactly.

Fix: the app now has a typed `KeyboardLayoutDecision` state, a dedicated
instruction revision, an immutable confirmation receipt, idempotent retry, and
irreversible conflicting-decision rejection. The receipt is part of the model
artifact fingerprint and replay applies the source receipt rather than
manufacturing a new one.

Permanent regressions include:

- `hardware_lease_requires_one_persisted_layout_confirmation`
- `layout_confirmation_uses_its_dedicated_revision_across_unrelated_state_updates`
- `confirmed_layout_decision_is_idempotent_and_cannot_flip_to_rejection`
- `rejected_layout_decision_is_typed_terminal_idempotent_and_irreversible`
- `http_layout_confirmation_uses_connection_accept_time`
- `fitting_checkpoint_refuses_to_infer_layout_without_a_confirmation_receipt`
- `layout_confirmation_receipt_is_part_of_the_artifact_fingerprint`
- `recorded_replay_preserves_the_source_layout_confirmation_receipt`

Crabbox evidence: the calibration-app, calibration-worker, artifact, and replay
contract targets passed together, followed by Rustfmt and Clippy.

## Finding 014: rejected capture could lose its footer-closed incident session

- Severity: High
- Status: Resolved
- Area: terminal calibration evidence

Strict finalization previously combined two different questions: whether the
session bytes were durably closed and whether those bytes were admissible as
normal evidence. A source mismatch or other rejected provenance could make
strict admission fail before the runner retained a footer-closed diagnostic
session.

Expected behavior: every runner-owned finite attempt closes its byte stream. A
rejected attempt remains permanently ineligible for fitting, but its bounded
diagnostic bytes and terminal cause survive.

Fix: `SessionWriter::finish_closed` now returns a footer-closed
`ClosedSession`; strict evidence admission is a separate operation. Incident
closure first ends label admission, closes the session, builds one bounded
canonical incident journal, and marks it non-evidence. It never weakens normal
session admission.

Permanent regressions:

- `rejected_provenance_keeps_footer_closed_bytes_for_incident_diagnosis`
- `abort_closes_label_admission_before_incident_finalization`
- `source_payload_mismatch_keeps_closed_diagnostic_bytes`

The targeted unit, session, hardware-calibration, calibration-app,
calibration-worker, and calibration-capture targets passed on the Crabbox.

## Finding 015: terminal control faults were not one durable final validation state

- Severity: High
- Status: Resolved
- Area: hardware-calibration finalization

Abort, browser-lease loss, source failure, and processing failure could take
different return paths. The app could also change after validation but before
the passed result was committed. Intermediate files alone did not provide one
typed answer about whether held-out validation passed, and a late write failure
could leave ambiguous state.

Expected behavior: one runner-owned commit path handles every terminal cause.
`CalibrationPhase::ValidationPassed` is rechecked against current app control
state at the final boundary.
`final-state.json` is written only after all normal or incident artifacts, so
its presence is the last durable outcome marker.

Fix: the runner now records typed terminal causes, rechecks abort, lease,
source, layout, and phase state in the final validation commit, persists a
bounded incident when possible, and writes typed `final-state.json` last. A
passing outcome requires `CalibrationPhase::ValidationPassed`, serialized as
`validation-passed`, and
`CalibrationSnapshot.heldout_validation_passed=true`. Failure uses
`CalibrationPhase::CalibrationFailed`, serialized as `calibration-failed`.
That persisted result is not live authority.
Background fit and validation workers are pure compute and cannot write around
this owner.

The historical regression names used pre-hard-cut status vocabulary. They are
intentionally omitted here until the final source names and combined Box closure
record settle. Finding 014 retains the footer-closure tests that do not depend
on result vocabulary.

## Finding 016: FIFO_STATUS2 was treated as a direct-register loss counter

- Severity: High
- Status: Resolved
- Area: sensor firmware and wire protocol

Rev A never configured `FIFO_CTRL5`, so the LSM6DS3TR-C remained in reset-default
FIFO bypass mode. Firmware nevertheless read `FIFO_STATUS2.OVER_RUN` and exposed
it as sample-loss evidence. That bit reports FIFO overrun; a disabled FIFO stays
empty and cannot report lost direct-output conversions.

Expected behavior: the direct-register architecture must not claim a loss
detector it does not configure. Unexpected software capacity, read, deadline,
and transport faults remain explicit.

Fix: firmware no longer reads `FIFO_STATUS2`. FIFO-overflow fields, counters,
and admission checks were removed. Protocol bit 0 is now unknown and rejected.
The corrected minimum transaction is 207 I2C clocks for one status poll, not
243, yielding the documented 64.584 percent minimum 400 kHz bus occupancy for
six 208 Hz sensors.

Permanent evidence includes
`removed_fifo_status_bit_is_not_part_of_the_current_protocol`, the portable
frame-protocol conformance case `removed FIFO flag rejected`, and the native
Zephyr register conformance path.

Crabbox evidence: the ten affected firmware, protocol, session, decoder,
neutral, capture, engine, and unit targets passed together as 10 of 10 under
PID `247159`. The selected left and right Zephyr firmware cross-build passed
under PID `235744`.

Physical limitation: direct-output registers still cannot prove that every
internal sensor conversion produced an observed interrupt and read.

## Finding 017: nominal sensor cadence deterministically faulted its own cache

- Severity: Critical
- Status: Resolved
- Area: acquisition cadence and fault semantics

Six sensors produced at 208 Hz while aggregates consumed one cached sample per
sensor at 200 Hz. The old one-slot cache marked any unread replacement as
`acquisition_overrun`. Even ideal hardware would therefore report about 8
false overruns per second per sensor, 48 per second per hand, or 172,800 per
hand-hour.

Expected behavior: the faster source deliberately feeds a latest-sample 200 Hz
grid. Normal 208-to-200 replacement is decimation, not a fault. A selected
sample retains its exact source-edge timestamp so skipped samples remain
visible.

Fix: replacing one clean unread cache entry now keeps only the newest complete
sample without setting overrun. Pending-read and work-submit collisions,
bus/read/config errors, stale samples, deadlines, invalid offsets, and USB-slot
failures still latch acquisition faults. Snapshot consumption clears reported
fault state only at its defined boundary.

The native conformance program proves clean replacement selects the newest
edge, remains non-overrun, cannot mutate a finished snapshot, and keeps real
faults distinct. It ran in the 10-of-10 affected Crabbox suite under PID
`247159`.

Physical limitation: oscillator variance, actual decimation rate, and capture
of every approximately 75 us DRDY pulse remain Gate 0A measurements.

## Finding 018: Gate 0A lacked a decisive native read-to-frame proof

- Severity: High
- Status: Resolved
- Area: native analyzer capture and reduction

The old path accepted a typed external evidence document and used payload
equality to connect analyzer reads to host frames. A human-authored aggregate
could stand between the capture and scorer. Identical consecutive motion bytes
made linkage ambiguous, the contract lacked an observable commit boundary, and
it treated every well-formed read absent from a frame as unexplained even when
the 208-to-200 schedule deliberately decimated it.

Expected behavior: the decisive scorer reads every native little-endian `u16`
sample word. The full capture accounts for every observed INT1 edge, I2C read
or explicit collision, D9 eligibility fall, D8 snapshot, selected read, and
decimated read exactly once. Each frame selects the newest completed eligible
read for each sensor at one observable boundary. No external JSON, decoded
NDJSON, or human aggregate can replace the raw trace.

Fix: Rev A configures pulsed DRDY plus D9/P1.14 cache-commit and D8/P1.13
aggregate-snapshot markers. After I2C, the worker holds cache then sensor state,
raises D9, stores and latches one slot, holds the marker for at least 2 us,
lowers it, and clears pending while state remains locked. Successful D9 fall is
eligibility; a same-sensor INT1 before that fall invalidates the capture. Main
raises D8 before the locked six-slot copy, lowers it afterward, and preserves
the immutable copied array during encoding. D8 rise is selection.

The only decisive artifact is one DSLogic Plus ten-bit raw capture at 2 MHz:
7,220,000,000 words, 14,440,000,000 bytes, and at least 3,601 scored seconds.
The reducer derives the exact boot anchor, validated quiet initialization and
arming, admitted I2C reads, marker order, per-sensor ordinals, decimation phase,
pre-grid decimation, selected-read age, and host linkage from the raw waveform.
The portable C startup transcript supplies an independent executable producer
for its startup regression. The 20 MHz 10-second preflight and 2 MHz 10-minute soak remain
non-decisive. Build, finalization, hashing, and scoring run in a Crabbox; only
physical USB capture runs on the analyzer host.

Permanent regressions include:

- `raw_waveform_derives_every_read_and_links_host_frames`
- `boot_marker_bus_decode_and_marker_order_are_waveform_derived`
- `full_capture_read_rules_reject_semantic_mutations`
- `cache_replacement_derives_decimation_and_phase`
- `raw_format_rejects_an_incomplete_u16_word`
- `fixed_capture_settings_are_not_sidecar_choices`
- `score_gate0a_requires_native_capture_and_never_overwrites_a_report`
- `score_gate0a_reduces_raw_samples_and_writes_a_bound_report`

The capture-stack focused suite passed 15 tests on dated Crabbox
`bx_v97q4knd`, including the sparse 14.44 GB wrapper contract. This report does
not claim a physical raw capture or a final frozen repository-wide validation.

Physical limitation: generated raw traces do not prove real D8, D9, INT1, I2C,
or USB timing. The full physical raw capture remains mandatory. D8 is not a
host-delivery marker because the analyzer cannot observe USB or host-kernel
backlog, DTR, or the kernel read boundary. The pinned DSLogic firmware blobs
also lack a reproducible source mapping, which remains a legal and supply-chain
risk.

## Finding 019: the only native uinput lane could silently skip

- Severity: High
- Status: Resolved
- Area: Linux output acceptance

The portable `output-system-contract-test` intentionally skipped when
`/dev/uinput` was missing or inaccessible. A green general suite could therefore
contain no real kernel output evidence while the roadmap described native
output as complete.

Expected behavior: portable suites may skip host capability, but one explicit
acceptance target must turn every such skip into failure and expose the real
device node.

Fix: `//:output-system-required-test` sets
`INVISIBLE_KEYBOARD_REQUIRE_UINPUT=1`, is Linux-only and manual, and runs with
`no-sandbox`. Its four tests require exact Core-28 identity and access, 10,000
kernel tap pairs, an independent evdev consumer, fault-release cycles, and
SIGKILL device destruction without mislabeling that destruction as a 250 ms
all-up guarantee.

Permanent tests:

- `native_probe_verifies_core28_identity_and_non_root_access`
- `native_kernel_stress_executes_ten_thousand_pairs_and_finishes_neutral`
- `native_evdev_application_observes_every_core28_pair`
- `sigkill_destroys_the_owned_uinput_device_without_a_250ms_claim`

Crabbox evidence: the required target passed 4 of 4. The stress observed 10,000
pairs, 10 release cycles, 20,020 key events, p95 14 us, p99 18 us, and zero
stuck keys.

## Finding 020: evdev 0.13.2 emitted the wrong UI_SET_PHYS ioctl

- Severity: High
- Status: Resolved
- Area: Linux virtual-device identity

The required native lane exposed a dependency defect: evdev 0.13.2 encoded
`UI_SET_PHYS` with a one-byte ioctl size (`0x4001556c`). The kernel expects the
pointer-sized encoding (`0x4008556c` on the tested 64-bit Box) and returned
`EINVAL`, so the required physical-path identity could not be set.

Expected behavior: keep the exact physical identity
`invisible-keyboard/core28`; do not drop it to make the test pass.

Fix: the Bazel dependency graph applies the minimal upstream
[correction commit](https://github.com/emberian/evdev/commit/7fee138a341f73b81b9c4f8c59e3527c6a169ea3),
reviewed in upstream [PR #134](https://github.com/emberian/evdev/pull/134).
The local patch is
`third_party/evdev-0.13.2-ui-set-phys-pointer-size.patch` and can be removed once
a released crate contains that commit.

Crabbox evidence: the required probe observed `BUS_VIRTUAL 0000:0000`, version
1, physical path `invisible-keyboard/core28`, values `[1, 0]`, effective UID
1000, and zero stuck keys. Lockfile, Rustfmt, Clippy, and udev syntax checks also
passed.

Deployment limitation: the checked-in group and udev policy was syntax-checked
but not installed on the shared Box. A deployment host still needs that setup.

## Finding 021: axis normalization lacked an end-to-end calibration proof

- Severity: High
- Status: Resolved
- Area: raw-axis canonicalization

The manifest and frame canonicalizer proved that one signed permutation could
rotate one frame, but no test proved that non-identity raw wire axes produced
the same fitted model, held-out predictions, confusion matrix, and validation
canonical axes. Either calibration boundary could therefore omit the required
one-time transform without a failing end-to-end contract.

Expected behavior: every raw vector rotates exactly once after identity binding
and before fitting, held-out validation, replay, or live decoding.

Fix: `signed_permutation_wire_axes_are_equivalent_through_fit_and_held_out_validation`
independently encodes canonical fixtures into non-identity wire axes, runs both
fitting and held-out evaluation, and compares decoded frames, model behavior,
causal predictions, counts, metrics, and validation disposition. Negative controls prove
that removing either normalization boundary breaks equivalence.

Crabbox evidence: `//:calibration-engine-contract-test` passed 15 of 15 under
PID `209136`. Separate fitting and held-out mutation workspaces failed at their
intended decoder and causal-prediction equality assertions under PIDs `213653`
and `213900`.

## Finding 022: identical IMU branch swaps were claimed as machine-detectable

- Severity: Medium
- Status: Resolved as a process gate
- Area: physical provenance boundary

All six LSM6DS3TR-C breakouts report the same `WHO_AM_I` value and expose no
per-board identity used by this design. A static manifest can detect a changed
declared mount or wiring identity, but it cannot know that two physically
identical labeled branches were swapped while their declarations stayed the
same.

Expected behavior: state this limit instead of claiming an automatic check.
Treat every disconnect, move, or suspected swap as a new physical provenance
event.

Fix: the assembly and roadmap now require permanent branch labels, point-to-
point continuity records, mounted photo hashes, a new mount ID, and a repeated
six-face axis map after any disconnect or swap. The typed manifest still
rejects duplicate or reordered declared identities through
`sensor_declarations_follow_canonical_index_and_role_order` and
`sensor_mount_wiring_and_interrupt_identities_are_required_and_unique`.

Physical limitation: this resolution is a human-inspected process gate, not a
new machine identifier. It remains unpassed until the rig is assembled and
inspected.

## Finding 023: capture and fit disagreed on what a clean stream meant

- Severity: High
- Status: Resolved
- Area: calibration stream admission

Capture and fitting maintained separate clean-stream predicates. The capture
predicate omitted device-time and host-time regressions even though fitting
rejected both. A session could therefore pass the earlier checkpoint and fail
later for an integrity fact that was already present.

Expected behavior: one architectural contract defines a clean two-hand sensor
stream and every calibration boundary uses it.

Fix: `SessionSummary::is_clean_two_hand_sensor_stream` now owns the shared
predicate. It requires both populated streams and rejects invalid records,
gaps, duplicates, reorder, device or host regression, invalid sensor masks,
clock-unsynced frames, and acquisition overruns. Capture and fitting call the
same method.

Permanent regression:
`clean_two_hand_stream_rejects_recorded_time_regressions` constructs real
audited sessions for the clean, device-regressing, and host-regressing cases.
It passed inside the firmware hard-cut's 10-of-10 Crabbox suite under PID
`247159`.

## Finding 024: one stalled HTTP request could overflow controller channels

- Severity: Medium
- Status: Resolved
- Area: calibration service scheduling

The two-second loopback request timeout fixed an infinite stall, but it still
allowed about 832 controller frames at the planned two-hand 208 Hz input rate
while the foreground runner was blocked. That exceeds the bounded 256-event
channel and could manufacture a source failure from a partial browser request.

Expected behavior: one bounded bad request must time out while enough channel
capacity remains for the runner to resume draining controller input.

Fix: the HTTP connection timeout is now 100 ms. The permanent unit test
`partial_http_request_cannot_fill_the_controller_channel` derives the worst-case
stalled frame count from the actual timeout, two-controller ODR, and channel
capacity rather than copying a magic count.

The unit and five related terminal-finalization targets passed together on the
Crabbox.

## Finding 025: a one-hand rig was allowed to imply passed Core-28 validation

- Severity: High
- Status: Resolved
- Area: prototype admission boundary

The staged build begins with one hand for cost and falsification, but Core-28
contains outputs owned by both hands. Treating the first rig as an exact layout
calibration path would make half the advertised identity problem unobservable.

Expected behavior: the one-hand rig may test electrical integrity, timing,
contact observability, and within-hand exploratory evidence only. An armable
Core-28 artifact requires two independently admitted controllers and all twelve
sensors.

Fix: the roadmap and calibration contract now make this split explicit, and
the executable hardware path requires two distinct controllers, one keyboard,
and one output directory. The permanent CLI regression is
`hardware_calibration_requires_two_controllers_keyboard_and_output_directory`.
The CLI contract target passed on the Crabbox during the admission lane.

Physical limitation: neither one-hand nor two-hand contact and position
separability has been measured.

## Finding 026: live faults could bypass one neutral terminal-revocation path

- Severity: High
- Status: Resolved
- Area: live output and session shutdown

Reader errors, transport faults, disconnects, planned stop, and output errors
used several control-flow exits. A path that returned its first error without
driving the canonical live fault owner and session close could bypass all-up or
lose the final evidence boundary.

Expected behavior: every live terminal path records only the first failure,
terminally revokes the user-attested live instance through the same fail-neutral
runtime, drains or disconnects both sources as appropriate, closes the session,
and reports any later cleanup failure without replacing the original cause. No
later frame can rearm that instance.

Fix: the CLI now funnels transport results through shared first-error and
`fail_live_run` helpers, while the runtime owns release and terminal
wear-session revocation. The replay and CLI contracts cover output failure,
source loss, watchdog, identity and clock faults, disconnect, planned signals,
buffered-tap discard, busy controller named pipes, final neutral state, and
rejection of every later frame.

Permanent regressions include:

- `calibrated_live_output_failure_recovers_and_finishes_neutral`
- `live_disconnect_discards_pending_work_resets_clocks_and_finishes_neutral`
- `live_source_loss_revokes_the_instance_without_rearm`
- `live_watchdog_terminally_revokes_and_cannot_rearm`
- `live_watchdog_rejects_every_later_frame_without_new_confirmation`
- `live_planned_signals_stop_output_through_the_canonical_safety_owner`
- `live_disconnect_prevents_finish_from_emitting_a_buffered_tap`
- `calibrated_live_stop_finalizes_while_both_fifos_are_busy`

The replay contract passed in the layout-provenance Crabbox lane, and the CLI
contract passed in the runtime-admission lane.

## Finding 027: browser acknowledgment could stand in for physical contact

- Severity: High
- Status: Resolved
- Area: passive-surface observation truth

The rendered-cue receipt proved when an instruction was visible, but the older
flow could use that receipt as the target trigger without first proving that a
contact occurred or that the prompted hand and finger produced it. No contact,
a wrong-hand tap, a wrong-finger tap, and two contacts could therefore be
collapsed into one prompted label.

Expected behavior: the browser receipt opens a bounded response gate only. A
causal sensor observation, independent of position truth, must inspect both
hands and accept exactly one expected hand and finger. Every other outcome is
retained as a named rejection and cannot advance coverage.

Fix: the accepted rendered revision opens a provisional 0 to 1.5-second
response gate inside a 1.8-second target segment. A frozen causal contact rule
inspects both streams, selects the first qualified observation without using
the prompted position to choose a peak, and distinguishes correct contact, no
contact, wrong hand, wrong finger, and multiple contacts. Raw failed attempts
remain evidence.

Permanent regressions include:

- `first_event_rule_reports_wrong_hand_wrong_finger_no_event_and_multiple_events`
- `held_out_window_selection_is_independent_of_position_truth`
- `missing_causal_window_keeps_raw_attempts_out_of_matched_coverage`
- `repeated_labels_cannot_reuse_overlapping_causal_windows`

These tests are in the permanent Bazel targets
`//:unit-tests`, `//:calibration-capture-contract-test`, and
`//:hardware-calibration-contract-test`. This report does not invent a final
Box result for the current combined tree; the repository-wide frozen rerun
remains separate.

Physical limitation: generated contacts do not establish that real passive
taps yield one reliable causal peak or correct finger ownership.

## Finding 028: keyboard warm start could influence passive model admission

- Severity: High
- Status: Resolved
- Area: calibration domain separation

The earlier policy pooled keyboard and passive examples into position
centroids and chose its intent threshold from keyboard evidence. That allowed
switch mechanics to influence a passive-surface model even though the physical
keyboard does not prove desk contact. It also treated `B`, Space, and Backspace
as ordinary keyboard labels despite their ambiguous hand or incompatible thumb
contact path.

Expected behavior: keyboard warm start is a non-admission signal precheck.
`B`, Space, Backspace, and both outer thumbs are passive-only. Position
centroids and the final decoder threshold come from disjoint passive evidence,
and held-out evidence mutates neither the frozen collection trigger nor model.

Fix: the keyboard plan covers only the 29 keyboard-addressable positions. The
passive training plan supplies three centroid taps for all 34 positions and a
separate ten-target plus quiet-window threshold block. The collection trigger
freezes before passive fitting and stays distinct from the final decoder
threshold. Held-out evaluation reads both but cannot modify them.

Permanent regressions include:

- `keyboard_warm_start_excludes_ambiguous_and_passive_only_positions`
- `keyboard_warm_start_labels_cannot_change_passive_fitted_artifact`
- `passive_threshold_windows_cannot_claim_the_keyboard_domain`
- `held_out_validation_cannot_mutate_the_locked_artifact`
- `held_out_labels_cannot_change_continuous_candidate_generation`

The permanent targets are `//:layout-contract-test`,
`//:calibration-engine-contract-test`, and
`//:calibration-capture-contract-test`. No physical transfer result or final
combined Box result is claimed here.

## Finding 029: saved validation could be mistaken for live mount continuity

- Severity: High
- Status: Resolved
- Area: live wear-session authority

A passed held-out artifact could be interpreted as proof that both wearables
remained in the same position. The sensor stack cannot detect all removal,
movement, or loosening, so a persisted result cannot safely authorize a later
or restarted process by itself.

Expected behavior: the persisted result is
`CalibrationPhase::ValidationPassed`, serialized as `validation-passed`, with
`CalibrationSnapshot.heldout_validation_passed=true`; failure is
`CalibrationPhase::CalibrationFailed`, serialized as `calibration-failed`.
The resulting `HeldOutValidation` authorizes model/replay use only, while its
faults use `HeldOutValidationError`. The artifact requires wear-session
confirmation. Each live process needs a fresh explicit user attestation that
both wearables have not moved. Any source, watchdog, identity, clock, output,
or process fault terminally revokes that instance, forces neutral output, and
prevents silent rearm.

Fix: the artifact binds `requires_wear_session_confirmation=true`. Live
construction consumes a single-use `WearSessionConfirmation`, reports a
user-attested status, and owns terminal revocation. Watchdog, source loss,
clock regression, disconnect, output failure, planned stop, and process end
cannot return the instance to an armed state. Each restart repeats held-out
derivation from the artifact-bound plan, schema-v9 facts journal, clocks, and
raw session, compares the cached report, and then requires a new no-movement
confirmation. If either wearable moved, the user must revalidate before
confirmation.

Permanent regressions include:

- `artifact_round_trip_preserves_decoder_behavior_and_identity`
- `artifact_bound_two_hand_replay_emits_exact_core28_taps_and_neutralizes`
- `live_watchdog_terminally_revokes_and_cannot_rearm`
- `live_watchdog_rejects_every_later_frame_without_new_confirmation`
- `live_source_loss_revokes_the_instance_without_rearm`

The artifact and replay contracts are permanent Bazel targets. Their latest
combined-tree Box result is not asserted in this report. Physical limitation:
the design intentionally relies on an honest user attestation because sensors
cannot prove continuing wearable position.

## Finding 030: legacy result and layout meanings survived the hard cut

- Severity: High
- Status: Resolved
- Area: serialized calibration and layout contract

The earlier surface mixed saved validation with a claim about continuing
wearable placement and used Core-34 as if it were the current enabled layout.
That could let a caller or document infer live authority from a saved result or
treat reserved positions as enabled outputs.

Expected behavior: the enabled layout is Core-28 on a 34-position physical
grid. Core-34 is future only. `B`, Backspace, Space, and both outer thumbs use
the project-defined ownership in the layout manifest. Saved success is
`ValidationPassed` / `validation-passed` with
`heldout_validation_passed=true`; failure is `CalibrationFailed` /
`calibration-failed`. The artifact says that a separate wear-session
confirmation is required. No persisted state may claim continuing wearable
placement or live-output authority.

The final source uses only that contract. Permanent regressions include
`layout_prints_all_34_positions_and_core28_count`,
`frontend_contains_the_accessible_seven_screen_contract`,
`a_published_saved_result_is_read_only_until_exact_acknowledgement`, and
`artifact_round_trip_preserves_decoder_behavior_and_identity`.

## Finding 031: pending merge work had no common hard capacity

- Severity: High
- Status: Resolved
- Area: transport and runtime resource bounds

An adversarial publication order could retain work faster than a merge
watermark released it. Without one hard bound at every entry path, the device,
replay, or live path could grow memory instead of failing closed.

Fix: device transport, calibrated replay, and calibrated live merging each keep
at most 64 pending records. A 65th retained record is a typed capacity fault.
Replay forces neutral before returning. Live clears pending work and clocks,
forces neutral output, terminally revokes the instance, and rejects later
frames. Permanent regressions are
`pending_candidate_capacity_is_hard_and_overflow_is_atomic`,
`replay_pending_capacity_fails_closed_with_a_typed_error`, and
`live_pending_capacity_terminally_revokes_without_rearm`.

## Finding 032: a later cleanup fault could replace the first terminal cause

- Severity: High
- Status: Resolved
- Area: terminal calibration and live fault provenance

One attempt can encounter a source or control fault and then hit a second error
while closing the journal, releasing output, or writing terminal state. If the
second error becomes the reported disposition, the evidence no longer explains
why admission first stopped.

Fix: the first terminal cause is latched and remains authoritative. Cleanup and
incident-persistence errors are retained separately. Output is neutralized and
the durable terminal state is written last; a later failure cannot silently
reclassify or rearm the attempt. Permanent regressions include
`live_transport_fault_remains_primary_when_secondary_finalization_fails`,
`evidence_abstention_remains_primary_when_neutralization_fails`, and
`malformed_transport_remains_primary_when_neutralization_fails`.

## Finding 033: layout and saved-result receipts could acknowledge the wrong revision

- Severity: High
- Status: Resolved
- Area: browser-to-service receipt provenance

A general application revision or stale browser action could otherwise be
mistaken for acceptance of the exact layout instruction or saved terminal
result that the service meant to show.

Fix: layout confirmation and saved-result acknowledgment each use a dedicated
pending revision. The service accepts only that exact revision, rejects stale
or unrelated revisions, and retains the first accepted monotonic timestamp.
The result is durably saved before it is published for acknowledgment, and the
final state closes after acknowledgment or its finite timeout. These receipts
remain software facts, not hardware or wearable-continuity attestations.

Permanent regressions include
`layout_confirmation_uses_its_dedicated_revision_across_unrelated_state_updates`,
`a_published_saved_result_is_read_only_until_exact_acknowledgement`,
`saved_result_acknowledgement_uses_the_exact_bounded_deadline`, and
`timed_out_acknowledgement_is_durable_and_does_not_claim_the_result_was_seen`.

## Finding 034: a report-only object could forge held-out admission

- Severity: High
- Status: Resolved
- Area: held-out admission capability

Checking a structurally valid report or exposing a public constructor for an
admission token would let a caller bypass the raw evidence path even when the
report contained passing counts and the expected artifact hash.

Fix: a validation report is an output cache. Only engine evaluation over the
artifact-bound held-out plan, schema-v9 facts journal, clocks, manifest, and raw
session can create the opaque `HeldOutValidation` value. Runtime repeats that
derivation and compares the complete cached report before any controller or
output opens. Permanent regressions include
`runtime_recomputes_and_rejects_a_real_failed_heldout_capture` and
`runtime_validation_rejects_forged_and_non_neutral_reports_in_live_and_replay`.

## Finding 035: externally supplied derived evidence could bypass capture replay

- Severity: High
- Status: Resolved
- Area: fitting and validation input authority

The former lower-level API accepted a calibration-evidence JSON and standalone
matching-rule JSON. Because those files already contained selected and labeled
windows, supplying them to fitting or validation created a second authority
path around the capture journal and raw session.

Fix: fit and validation accept the canonical plan-bound capture journal, clocks,
manifest, and raw session and derive evidence internally. Diagnostic evidence
can still be serialized for inspection, and the schema-v4 artifact fingerprints
its derived fitting evidence, but no input parser or CLI position can use that
file to admit a model. Permanent regressions include
`diagnostic_evidence_json_cannot_enter_trusted_engine_apis`,
`runtime_ignores_missing_corrupt_and_replaced_diagnostic_evidence`, and
`calibrated_replay_runs_only_after_recomputing_the_canonical_heldout_bundle`.
The final CLI help accepts plan, journal, clock, manifest, raw session, and
neutral-report inputs only.

## Finding 036: capture facts could be relabeled with a different surface plan

- Severity: High
- Status: Resolved
- Area: prompted-surface truth provenance

A facts journal that trusted caller-supplied target labels or lacked a complete
plan binding could be paired with another target table. Correct sensor windows
could then acquire different key or null meanings without changing the raw
session.

Fix: schema-v9 journals bind the one canonical fitting plan or the held-out plan
reconstructed from the locked artifact and run seed. Prompt records retain IDs
and presentation receipts, while the plan owns target meaning. Admission checks
purpose, plan hash, artifact binding, exact order, completeness, and missing or
extra facts before any evidence is derived. Permanent regressions include
`same_finger_a_to_q_prompt_relabel_is_rejected_by_plan_order`,
`surface_facts_reject_unknown_duplicate_missing_and_reordered_prompt_ids`,
`purpose_plan_artifact_and_seed_bindings_are_exact`,
`persisted_plan_hash_cannot_be_rebound_after_capture`,
`held_out_plan_rejects_cross_artifact_reuse`, and
`held_out_plan_rejects_seed_mutation`.

## Finding 037: real-time CLI tests competed with CPU-heavy calibration tests

- Severity: Medium
- Status: Resolved
- Area: Bazel test scheduling

The CLI target drives two real FIFOs and asserts that the causal online clocks
arm. A cold whole-graph run scheduled it beside the long calibration engine
target. Host starvation pushed one valid fail-closed holdover 3.6 ms beyond the
250 ms limit, so a success-path test failed even though the runtime correctly
kept output disarmed. Seven deliberate CPU burners reproduced both safe
outcomes: no arming, or a terminal extrapolation error.

Fix: `//:cli-contract-test` is an exclusive Bazel target, and its Rust harness
runs with `--test-threads=1`. The first change isolates the binary from other
Bazel tests. The second prevents its own 482,501-record resource case from
starving its real-FIFO timing cases. Neither change alters a production timeout
or removes an overload test. The dedicated clock contract still proves the
exact 250 ms holdover boundary, while deliberate overload remains expected to
stay disarmed or fail closed. At the time of this fix, the then-48-case CLI
suite passed serially in 193.8 seconds before that dated graph run.

An earlier pre-v6 Crabbox pass on `bx_v97q4knd` ran all 35 non-manual targets
and reported 44 CLI cases. A later dated target contained 48 cases. Neither
historical count describes the current target; the final validation report
owns the frozen source's exact pass count and graph inventory.

## Finding 038: one FIFO test producer invented source silence

- Severity: Medium
- Status: Resolved
- Area: live shutdown stress validity

The busy-controller shutdown regression wrote left and right FIFO traffic from
one producer thread. If one FIFO blocked, the producer stopped servicing the
other FIFO. Under backpressure, the test could therefore manufacture a silent
controller that the real two-reader runtime correctly rejected.

Fix: the fixture now gives each hand an independent continuous writer. One
blocked FIFO cannot stop the other source. The causal shutdown path, bounded
reader admission, source-silence bound, and clock holdover remain unchanged.
Evidence finalization intentionally has no universal timeout. The focused test
`calibrated_live_stop_finalizes_while_both_fifos_are_busy` passed ten
serial runs after the repair. The final frozen-tree report records the larger
soak rather than treating this pre-freeze count as release proof.

## Finding 039: failed neutralization could be hidden behind neutral copy

- Severity: High
- Status: Resolved
- Area: terminal output truthfulness

The runtime could preserve an operational fault while discarding a later
all-up failure. The CLI then inferred neutrality from an empty software key set
and printed success even when the adapter had not confirmed release.

Fix: `RuntimeOutputError` retains the attempted event, primary adapter cause,
and independent recovery failure. Live and replay results preserve the first
operational cause and a separate neutralization failure. The report exposes
`output_neutrality_confirmed`, and the CLI prints neutral success copy only
when the adapter confirmed all-up. Otherwise it tells the operator to
disconnect the virtual input device before restart.

Permanent regressions include:

- `malformed_transport_remains_primary_when_neutralization_fails`
- `evidence_abstention_remains_primary_when_neutralization_fails`
- `adapter_failure_preserves_the_cause_and_independent_recovery_failure`
- `calibrated_live_preserves_transport_primary_and_reports_failed_all_up`
- `calibrated_replay_preserves_output_cause_and_independent_recovery_failure`

## Finding 040: monotonic receipts did not prove the canonical schedule

- Severity: High
- Status: Resolved
- Area: prompted-surface timing authority

The facts journal bound a surface plan and required increasing receipts, but it
did not prove that preparation, cue, contact, null, and motion facts occurred
at the plan's actual offsets. A compressed or shifted timeline could remain
internally ordered while changing the meaning of the captured sensor windows.

Fix: schema-v9 replaces the cue-only fact with a complete
`DisplayAcknowledged` chain. Admission reconstructs each presentation from the
bound run start, canonical segment offset, and cumulative render delay. It
requires the exact occurrence order and revision, then admits facts only in
their shifted half-open plan intervals. Missing preparation receipts,
compressed timing, early or late facts, reordered receipts, and incorrect
cumulative shifts fail before evidence derivation.

Permanent regressions include
`compressed_surface_timeline_is_rejected_even_when_each_receipt_is_internally_ordered`
and the complete schema-v9 capture, worker, replay, and CLI fixtures.

## Finding 041: selected truth windows could reuse raw records

- Severity: High
- Status: Resolved
- Area: fitting and held-out evidence independence

The selector marked one target time as used but did not reserve every source
record in its 16-frame truth window. Crafted same-session targets could
therefore share raw same-hand frames across centroid, threshold target, quiet,
or ordinary-motion truth. This was an admission-integrity defect, not
cross-run leakage. Fitting and held-out sessions are separate, and simultaneous
opposite-hand windows have distinct source records.

Fix: each fitting match and each held-out match owns its own full-window
reservation set. Every selector reserves all 16 actual source record ordinals.
The engine then independently reconstructs the chosen same-hand slices and
rejects any intersection with `TruthWindowRecordOverlap`; it does not trust the
selector's bookkeeping. The continuous decoder may still read held-out sensor
frames to make predictions because truth metadata, not sensor input, is the
scoring-only boundary.

Permanent regressions include
`derived_truth_windows_are_record_disjoint_within_each_session`,
`binder_rejects_intersecting_target_truth_windows`,
`binder_rejects_target_intersection_with_threshold_null_truth`, and
`binder_allows_simultaneous_opposite_hand_truth_windows`.

## Finding 042: bounded parsers still allocated whole persisted inputs

- Severity: High
- Status: Resolved
- Area: persisted-input and long-session resource bounds

Several JSON paths checked their parser cap only after reading the whole file.
Live shutdown also loaded the complete session again for audit and offline
clock fitting. A sparse oversized input or a healthy long recording could
therefore consume memory in proportion to its declared duration.

Fix: bounded JSON loaders now check metadata first and read at most the cap plus
one byte. Calibration uses its exact 1,200-second cap of 92,640,080 bytes, and
Gate 0A uses its exact 3,610-second cap of 278,692,316 bytes, including the
1,444,000 acquisition records plus the request and receipt control records.
Generic verify, clock fitting, and live finalization have no short duration
cap: one canonical
streaming parser audits, hashes, and accounts for the session, while exact
clock statistics use private 16-byte timestamp spools and four 16-bit radix
passes. Spools use owner-only mode on Unix and are removed on normal and error
returns.

Permanent regressions cover exact in-memory parity, corruption at the footer,
one-terabyte sparse admission failures before allocation, setup and Gate 0A
cap arithmetic, and a healthy session beyond the setup cap. The dated
pre-freeze resource run processed 92,640,272 bytes and 482,501 records with
7,819,264 bytes peak RSS and 6,294,027 source bytes per second. Its contract is
at most 64 MiB RSS and at least 2 MiB per second. The final frozen-tree report
must reproduce the release result against one immutable source identity.

## Finding 043: the canonical Gate 0A Bazel build ignored its runfiles

- Severity: High
- Status: Resolved
- Area: capture-stack build and release reproducibility

The `build-gate0a-capture-stack` target declared its patch and provenance as
Bazel data, but the shell recipe derived a repository root from
`BASH_SOURCE[0]`. Under `bazel run`, Bazel entered the staged executable from
`bazel-out`, so the recipe searched beside the output tree and failed before
patching or compilation. A direct shell invocation had hidden the broken
canonical entrypoint.

Fix: the Bazel target now exports the exact `rlocationpath` for each declared
input and uses the rules-shell launcher to export the runfiles environment.
The recipe resolves only those runfiles and no longer guesses a repository
root. The guide removes direct shell compilation and forwards the firmware
bundle with `--test_env`, so a skipped integration case cannot count as a
pass.

The permanent regression
`test_bazel_build_entrypoint_resolves_declared_inputs` executes the real
Bazel-produced entrypoint, proves both inputs resolve, and stops at the
Crabbox authorization guard before any upstream build. The 16-test focused
contract passed with the real firmware bundle. A fresh canonical `bazel run`
then completed the entire pinned stack, and its executable, firmware tar,
patch, and build-info hashes matched the previous two-build baseline byte for
byte. The replacement frozen-tree report must repeat the qualifying build
against its new source identity.

## Finding 044: selected-board ELFs embedded executor-specific debug paths

- Severity: High
- Status: Resolved
- Area: firmware release reproducibility and provenance

Two clean selected-board builds produced identical HEX and UF2 images but
different ELF files and provenance. The differing ELF bytes were confined to
DWARF sections: `.debug_info`, `.debug_line`, and `.debug_str` retained Bazel
output-base and temporary-build paths. Stripping debug made the ELFs match,
but publishing stripped artifacts would remove the symbols and source mapping
needed during board bring-up and would hide the declared-output defect.

Fix: the canonical build now passes `-fdebug-prefix-map` for the repository,
cache, and temporary build roots through Zephyr's C, C++, and assembly flag
interfaces. The loadable firmware is unchanged. After copying each ELF, the
build scans its complete bytes for every real executor root and fails before
writing provenance if any path remains.

Two fresh, independent pre-freeze builds then matched all seven declared
outputs byte for byte. Both ELFs retained eight debug sections with nonempty
`.debug_info`, `.debug_line`, and `.debug_str`; the fail-closed path scans
passed; and both HEX and UF2 pairs matched the earlier loadable-image
baseline. The replacement frozen-tree report must repeat this proof against
the new source identity.

## Finding 045: one shutdown deadline mixed safety with durable evidence

- Severity: Critical
- Status: Resolved
- Area: live shutdown claim boundary

The withdrawn v5 freeze treated the whole shutdown as though it had one
universal two-second completion bound. That mixed two different outcomes. The
output owner can and must attempt all-up immediately, but session size, storage
progress, strict audit, exact clock fitting, and report sync do not have a
portable fixed wall-clock bound. One timeout in a 50-run pre-freeze stress made
the contradiction visible. Passing later repetitions could not repair the
claim.

Fix: the contract now has two milestones. Adapter neutralization is measured
independently against the 250 ms Gate 0S target. Durable evidence completion has
no universal wall-clock promise. The CLI performs all-up before reader
coordination or evidence I/O, then finishes evidence under an infrastructure
watchdog used only by tests. No stdout line acts as the neutralization oracle.
The v5 source archive remains a no-ship historical artifact.

## Finding 046: non-signal terminal faults could discard admitted input

- Severity: High
- Status: Resolved
- Area: live reader cutoff and evidence completeness

The planned-signal path closed and drained the reader mailbox, but a transport,
clock, decoder, or output fault could leave already-admitted chunks behind.
Those chunks could disappear from the terminal session even though the output
path had correctly stopped.

Fix: every terminal cause closes the bounded mailbox atomically and drains its
admitted backlog through the evidence-only path. No drained candidate can
reach the decoder or output again. Reader-local chunks that had not published
before close are rejected. A successful FIFO write proves kernel acceptance,
not mailbox admission, so integration tests require the durable session to be
one exact gap-free admitted prefix. Mailbox unit tests prove that every
successful publish is received or drained and every post-close publish is
rejected.

## Finding 047: a live clock report did not bind exact session bytes

- Severity: High
- Status: Resolved
- Area: live evidence identity

Live-clock-report-v1 bound the acquisition manifest and session ID from the
header. Another footer-valid session with the same header could therefore be
paired with the report without changing either field.

Fix: live-clock-report-v2 adds `session_sha256`, derived from the strict
complete-session audit. Construction requires `SessionAudit`, and binding
checks both header identity and the complete byte hash. A permanent negative
test creates two valid sessions with the same header but different records and
proves that report replacement fails with `SessionContentMismatch`.

## Finding 048: status I/O could strand ownership or obscure durable success

- Severity: High
- Status: Resolved
- Area: live status ordering

Startup status was written after constructing the output owner and evidence
session. A blocked consumer could therefore strand live state before the run
began. Terminal status also lacked a contract for the case where evidence was
already durable but the status pipe failed.

Fix: startup status and its warning are flushed before the output adapter or
session exists. All terminal status is attempted only after the manifest,
session, and v2 report are complete and synced. If that write fails, the CLI
returns a typed error that says the live bundle is durable but status
publication failed and preserves any earlier primary fault. The regression
`terminal_status_disconnect_cannot_erase_a_durable_live_bundle` closes the real
stdout pipe, then strictly re-audits the surviving bundle.

## Finding 049: recording-mode live output retained an unbounded event vector

- Severity: High
- Status: Resolved
- Area: long-run memory bounds

The recording adapter used for live proof retained every output event even
though the durable session already owned the evidence. That made process memory
grow with an unbounded live run and undermined the streaming session design.

Fix: `LiveRecordingOutput` is now a zero-history sink that retains only its
next receipt ordinal. Session audit and exact clock fitting remain streaming.
The long-session contract keeps a 64 MiB RSS ceiling and a 2 MiB/s source
throughput floor. Two early full-graph executions reported roughly 75 to 77 MiB
while isolated and fifteen later uncached executions stayed near 7.8 MiB; the
old tree now also stays near 7.7 MiB. No O(n) production allocation was found.
The ceiling remains unchanged, and a breach now captures `/proc/status`,
`smaps_rollup`, maps, cgroup, executable hash, kernel version, and selected
allocator environment only on failure.

## Finding 050: one 250 ms test observed adapter-owned state, not an application

- Severity: High
- Status: Resolved
- Area: Linux output oracle

The planned-shutdown test queried key state through the adapter's own grabbed
event reader and described that as application observation. This crossed the
kernel but did not establish an independent consumer boundary.

Fix: the required uinput test opens a second evdev `Device`, exclusively grabs
the event node before transferring the adapter to `OutputSafetySession`, and
requires that independent consumer to receive both KEY_A down and KEY_A up. Ten
SIGINT cycles must publish the up receipt within 250 ms. The same lane verifies
the exact virtual-device identity, 10,000 kernel pairs, 10,000 independently
observed application-path pairs, and SIGKILL device destruction. This remains a
compositional native-evdev proof, not a compositor, focus, IME, or complete
`run-calibrated uinput` proof.

## Finding 051: durable publication ordering lacked an executable syscall oracle

- Severity: High
- Status: Resolved
- Area: Linux durability proof

Source inspection and Rust-level tests established the intended order but did
not independently observe the operating-system calls. A refactor could remove
a file or parent-directory sync while leaving high-level bundle tests green on
a warm filesystem.

Fix: the manual `//:live-durability-syscall-test` runs the real CLI under
`strace`, reconstructs interleaved unfinished calls, and rejects any order that
does not contain manifest file and parent sync, `END3`, session file and parent
sync, report file and parent sync, then neutral and durable status. It also
recomputes the v2 report's manifest and exact-session hashes. The first current
source run passed both current-boot and reset-boot shutdowns with CLI SHA-256
`da22538171d4ad136a5fa751392c191faeec8728bb790ff5b1cca51d7a74fe9f`
and trace SHA-256
`61fd51320828f5ca9d06b99f39d95a9f272ad6e8be48db3e737ab5e267d7299a`.
The final freeze must rerun it because any source edit changes the binary hash.
This proves Linux syscall order under `fsync`; it does not prove storage
hardware power-loss behavior or another operating system.

## Finding 052: late signal precedence lacked an exact regression

- Severity: Medium
- Status: Resolved
- Area: terminal cause immutability

The source conditions made the first cause immutable, but no end-to-end test
proved that SIGINT arriving after a source fault could not retroactively create
a planned-shutdown receipt. A naive attempt to manufacture a long finalization
window accelerated input above 200 Hz and correctly tripped the 64-candidate
merge cap, so it was rejected as an invalid test design.

Fix: the regression sends valid paced two-source traffic, stops the whole
process while both sources remain valid, closes both writers, and advances the
process in short SIGCONT/SIGSTOP slices. Once `END3` is visible while stopped,
it queues SIGINT and resumes. The final bundle must strictly audit, the original
transport fault must remain primary, and stdout must not claim
`shutdown_signal=interrupt`. This passed in 14.24 seconds without a production
hook or relaxed transport bound.

## Finding 053: held-out cues could be acknowledged behind the result screen

- Severity: High
- Status: Resolved
- Area: calibration presentation truth

`SurfaceValidate` selected the validation-result screen even though the
hardware runner continued to publish receipt-required held-out surface cues.
The browser acknowledged any pending instruction after two paint frames without
checking which screen was visible. A hidden surface cue could therefore receive
a valid service timestamp while the user saw only that validation was running.

Fix: `SurfaceValidate` now stays on the surface-transfer screen. That screen
labels fitting, model-locking, and held-out work separately; its count is named
as training coverage so it cannot imply held-out progress. The browser sends a
receipt only when the snapshot names the surface screen, that screen is still
visible after two paint frames, the pending revision is unchanged, and the
rendered cue text still matches the snapshot. The result screen appears only
after validation passes, fails, or the attempt terminates.

Permanent coverage extends
`hardware_worker_events_drive_the_same_finite_seven_screen_state_machine` with
a receipt-required held-out target and terminal screen transition. The frontend
contract also pins the visibility and cue-text guards. A final real-browser
traversal remains part of the frozen proof.

## Post-fix spot checks

The following outcomes were verified manually after rebuilding:

```text
malformed request -> HTTP 400, next state request -> HTTP 200
idle request -> HTTP 408, next state request -> HTTP 200
5000-byte JSON body -> HTTP 400, service remains live
wrong token -> HTTP 403
wrong Origin -> HTTP 403
unknown action -> HTTP 400
truncated session -> nonzero exit with exact record ordinal
existing session destination -> nonzero exit without overwrite
undeclared Right frames -> nonzero verification and clock-fit exits
```

These checks prove the software behavior only. They do not supply evidence
about contact observability, key separability, real calibration time, WPM,
false activations, comfort, radio behavior, or durability.
