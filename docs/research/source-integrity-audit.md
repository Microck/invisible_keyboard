# Source integrity audit

Date: 2026-08-29

## Scope and method

This audit asks whether the implementation-driving claims in the research,
firmware, protocol, calibration flow, and roadmap follow from their cited
sources. It does not test physical key separability.

The review used three independent lanes:

1. ChatGPT Deep Research through Oracle, using the primary project documents
   and reduced attachments after the first large upload timed out.
2. Direct inspection of the pinned LapCal snapshot and this repository.
3. Reconciliation against manufacturer, kernel, paper, license, and statutory
   primary sources.

The successful Oracle report and transcript are retained in the local session
under `~/.oracle/sessions/invisible-keyboard-source-audit-retry/artifacts/`.
Ordinary search was not substituted for the required deep-research run.

## Audit verdict

The architecture is implementable as a falsification rig, but source review
found two deterministic defects in the earlier acquisition contract:

- firmware interpreted a FIFO flag while the sensor FIFO remained disabled;
- nominal 208 Hz production fed a 200 Hz one-slot consumer while expected
  replacement was classified as a fault.

Both defects are now hard-cut from the canonical source and documents. The
corrected design uses pulsed data-ready, direct-register reads, intentional
latest-sample decimation, a D9 cache-commit marker, and a later D8 atomic
six-sensor snapshot marker. Gate 0A still requires physical evidence. No source
proves that the wearable signals separate every same-finger Core-28 key.

## P0 correction: FIFO status was not loss evidence

The LSM6DS3TR-C reset state leaves `FIFO_CTRL5.FIFO_MODE=000`. ST defines this
as bypass mode: the FIFO is disabled, not operational, and remains empty.
`FIFO_STATUS2.OVER_RUN` reports FIFO overrun. It is not a counter for direct
output-register samples overwritten before software reads them. `CTRL3_C.BDU`
protects multi-byte word coherence; it does not store sample history.

The earlier firmware never configured `FIFO_CTRL1` through `FIFO_CTRL5`, yet
read `FIFO_STATUS2` and propagated bit 6 as a wire fault. The corrected source:

- removes the status read and FIFO field;
- rejects former wire bit 0 as unknown;
- keeps clock-unsynced at bit 1 and acquisition-overrun at bit 2; and
- reserves acquisition-overrun for observable software, bus, deadline,
  freshness, offset, and transport failures.

Removing the false read changes the lower-bound I2C budget:

```text
mux write                     18 clocks
mux readback                  18 clocks
one STATUS_REG read           36 clocks
12-byte motion burst         135 clocks
total                        207 clocks/read

general case = 171 + 36N clocks/read for N status polls
207 * 6 * 208 / 400,000 = 64.584 percent minimum wire occupancy
6 * 207 / 400,000 + 60 us mux settling = 3.165 ms collision minimum
```

Each extra status poll across all six sensors adds 11.232 percentage points.
Four-poll collision service is about 4.965 ms with the implemented retry
delays, longer than one nominal 208 Hz period. Hardware must measure the real
poll distribution, clock stretching, capacitance, scheduling, and USB load.

Primary source:

- [ST LSM6DS3TR-C datasheet, DocID030071 Rev 3](https://www.st.com/resource/en/datasheet/lsm6ds3tr-c.pdf)

Relevant sections are FIFO behavior 5.5, FIFO bypass 5.5.1,
`FIFO_CTRL5` at 0x0A, `DRDY_PULSE_CFG_G` at 0x0B, `FIFO_STATUS2` at 0x3B,
and `CTRL3_C.BDU`.

## P0 correction: 208 Hz to 200 Hz needs explicit decimation

At nominal rates, each sensor produces eight more completed reads per second
than the aggregate stream selects:

```text
production                         208 reads/s/sensor
selection                          200 reads/s/sensor
intentional surplus                  8 reads/s/sensor
fraction of production             8 / 208 = 3.8461538 percent
one sensor-hour                     28,800 reads
one six-sensor hand-hour           172,800 reads
```

The corrected contract reads and timestamps every observed pulsed data-ready
event. On each 200 Hz boundary it atomically copies the newest completed sample
for all six sensors. A new completed read may replace an older read that no
aggregate selected. That replacement is deliberate latest-sample decimation,
not acquisition overrun.

Pulsed data-ready matters. In latched mode, a line can remain high while a new
conversion occurs, so an edge-triggered controller cannot observe that second
conversion. Rev A now sets `DRDY_PULSE_CFG_G=0x80`, giving an approximately
75 us pulse per observable conversion. Physical Gate 0A must still prove that
the GPIO path captures every pulse under collision and USB load.

Gate 0A uses D9/P1.14 as an analyzer-only cache-commit marker and D8/P1.13 as the
later aggregate-snapshot marker. Both use the same cache lock and their normal
pulses never overlap. After an I2C read, the worker raises D9 while it stores
and latches one cache slot, holds it for at least 2 us, and clears the pending
state only after attempting D9 low while the sensor-state lock remains held.
A successful D9 falling edge is the cache eligibility boundary. A physical
same-sensor INT1 before that fall invalidates the capture. At each 200 Hz
boundary, the aggregate path raises D8 before copying and consuming all six
slots, lowers it after the copy, and then unlocks. D8 rising is the selection
boundary and the copied array is immutable during encoding. The analyzer must
account for:

- full-capture per-sensor edge, selected-read, and decimated-read accounting;
- observed ODR and interval jitter;
- 250 us decimation-phase histograms and selected-read age;
- one monotonic per-sensor analyzer read ordinal;
- every read classified exactly once as selected or deliberately decimated;
- the selected read to be the newest unambiguous committed read at the D8
  marker;
- unique raw payload linkage to one host frame; and
- zero unexplained edges, unclassified reads, reused ordinals, stale
  selections, or decoder errors.

Before sensor I/O or interrupt enable, firmware emits the exact startup prefix
`D8 high >=20 us, both low >=20 us, D9 high >=20 us, both low >=20 us`. It is an
anchor only when it is the first activity and no INT1 or I2C event interrupts
it. The first normal D8 maps to sequence 0. Every later normal D8 advances the
sequence once even when DTR is absent, and reconnect never resets that
sequence. The host does not monitor DTR as a fault signal. Host-visible source
disconnects or transport faults still fail live output, while DTR never gates
the source timer.

The direct-register protocol cannot by itself prove that every internal sensor
conversion produced a captured pulse. That remains a physical evidence limit.

### Decisive analyzer artifact

Gate 0A does not trust a human timing summary, external evidence JSON, or
decoded NDJSON. The decisive scorer reads every native little-endian `u16`
word from a DSLogic Plus capture and its generated hash-bound sidecar. Bits 0
through 5 are the six INT1 lines, bit 6 is D8, bit 7 is D9, bit 8 is SDA, and
bit 9 is SCL. The full run requests 7,220,000,000 words at 2 MHz, produces
14,440,000,000 bytes, and admits no less than 3,601 scored seconds. A 20 MHz
10-second electrical preflight and a 2 MHz 10-minute transport soak are useful
but non-decisive.

The authoritative procedure is the capture guide's
[Canonical artifacts](../implementation/gate0a-capture-stack.md#canonical-artifacts),
[Box validation record](../implementation/gate0a-capture-stack.md#box-validation-record),
[Physical preflight sequence](../implementation/gate0a-capture-stack.md#physical-preflight-sequence),
[Physical-host capture](../implementation/gate0a-capture-stack.md#physical-host-capture),
[Crabbox finalization](../implementation/gate0a-capture-stack.md#crabbox-finalization), and
[GPL and firmware provenance](../implementation/gate0a-capture-stack.md#gpl-and-firmware-provenance)
sections. Build, finalization, hashing, and scoring run in a Crabbox. Only the
USB analyzer capture runs on its compatible physical host. The current process
pins the firmware blob bytes but cannot reproduce them from the available
source snapshots. That remains a legal and supply-chain risk.

The analyzer establishes source-side acquisition and selection order, not host
delivery time. USB device and host-kernel buffers may retain frames after D8.
Because the trace does not observe DTR or the kernel read boundary, it cannot
turn a D8 pulse into a no-backlog delivery claim.

## LapCal boundary

LapCal is valid prior-art and failure evidence. At pinned main commit
`8638759168d84d32fa50ae461e053055a01c3dff`, it supports a 34-key goal, five
three-position finger groups plus two thumb positions per hand, and a packed
five-byte-per-hand ordering. It does not supply a verified metric geometry or
a complete 34-position logical key map.

Core-28 plus six reserved slots is this project's design. Software tests prove
its internal mapping, not LapCal provenance or physical separability.

Immutable sources:

- [LapCal README](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org)
- [LapCal software format](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Software/README.org)
- [LapCal visualizer topology](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Utilities/keyboard-visualization/src/main.rs)
- [LapCal grouping diagram](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Documentation/diagrams/keyboard-state-format-grouping.svg)

The README's historical “about 10Hz at best” statement is in tension with
14.3-19.1 instrumented scan-boundary figures derived from stored logs. The two
boundaries are not shown to be directly comparable, so neither contradicts the
other.

## Calibration boundary

emg2qwerty supplies strong precedent for synchronized wearable signals and
physical-keyboard key-down/key-up labels, user-specific sessions, and remount
variation. It used two wrist sEMG devices and an Apple Magic Keyboard configured
for US English. The paper explicitly warns that physical-keyboard force can
create a domain mismatch for keyboard-free use.

It does not prove that proximal-finger IMUs transfer from keyboard presses to
passive-surface taps. Prompt counts, neutral duration, confidence thresholds,
and keyboard/passive pooling are provisional project choices until real data
passes held-out transfer.

The regular keyboard is therefore a non-admission warm-start precheck. Its
labels can test source alignment, signal presence, and expected finger
ownership, but they do not choose passive centroids or the final decoder
threshold. `B`, Space, Backspace, and both outer-thumb positions are
passive-only because their keyboard observation does not supply the required
unambiguous passive-surface hand/contact evidence.

The enabled terminology is Core-28 on a 34-position physical grid. The project,
not LapCal or generic QWERTY convention, assigns `B` to the left index finger,
Backspace to the left functional thumb, and Space to the right functional
thumb. Core-34 describes only a possible future output set.

A rendered browser cue is also not contact truth. Its acknowledgment only
opens a provisional 1.5-second response gate. The sensor stream must then
provide a causal contact observation. Both hands are inspected, and no
contact, wrong hand, wrong finger, or multiple contacts is an explicit
rejection. The target segment is 1.8 seconds so the retained evidence includes
the whole response gate and its causal context. These times are conservative
project choices, not achieved human-response measurements.

The schema-v9 capture journal records these observations as facts and binds the
one canonical fitting or artifact-bound held-out plan. That plan owns the
prompt-to-position and prompt-to-null meanings. Derived calibration evidence
and the validation report are diagnostic outputs, not independent authority
inputs. A schema-v4 fitted artifact retains both its fitting-journal hash and
the hash of the evidence derived from that journal; the second hash
fingerprints an output rather than opening another fitting path.

Journal, plan, manifest, clock, and raw-session hashes can prove internal
consistency. They cannot attest that a person saw the cue, wore the hardware as
declared, or performed the claimed contact. The capture service must preserve
that boundary instead of presenting a software receipt as physical truth.

Passive centroid fitting, final-threshold selection, and held-out scoring use
disjoint passive evidence. A frozen collection trigger is not the final
decoder threshold, and held-out data can mutate neither one. Ordinary-motion
exposure counts only measured motion-rich, non-contact, non-saturated windows.
The entire guided setup stops at 1,200 seconds and reports a named failure; it
does not collect until a claimed 100 percent result.

The persisted success state is `CalibrationPhase::ValidationPassed`, serialized
as `validation-passed`, with
`CalibrationSnapshot.heldout_validation_passed=true`. Failure is
`CalibrationPhase::CalibrationFailed`, serialized as `calibration-failed`.
The resulting `HeldOutValidation` authorizes the saved model and replay only;
its faults use `HeldOutValidationError`. It does not prove that the wearables
stayed in place. The artifact sets
`requires_wear_session_confirmation=true`, and live output needs a fresh
`WearSessionConfirmation` for each process based on the user's explicit
attestation that neither wearable moved. The system cannot detect removal,
movement, or loosening. Any source, watchdog, identity, clock, output, or
process fault terminally revokes that live instance. Restart reloads and
rederives validation from the artifact-bound held-out plan, schema-v9 journal,
clock fits, and raw session, and compares the cached report before any
controller or output opens. It then requires a new confirmation. If either
wearable moved, the user must revalidate before confirmation. There is no silent
rearm.

Primary source:

- [emg2qwerty NeurIPS 2024 paper](https://proceedings.neurips.cc/paper_files/paper/2024/file/a64d53074d011e49af1dfc72c332fe4b-Paper-Datasets_and_Benchmarks_Track.pdf)

## Linux output boundary

The Linux kernel documents uinput as a userspace virtual-input route. Normal
emission needs permission to open `/dev/uinput` for writing; root is not an API
requirement. Ownership, group mode, ACLs, and udev policy decide access. Reading
the created event node is required by this project's exclusive observer, not
by normal injection.

The 2026-08-29 Crabbox run exercised a real non-skipping kernel path as uid
1000. It verified the exact `BUS_VIRTUAL` identity and physical path, 10,000
down/up pairs, 20,020 total observed events including release cycles, and zero
stuck keys. This does not prove compositor focus, IME behavior, suspend/resume,
or another operating system.

Primary sources:

- [Linux kernel uinput guide](https://docs.kernel.org/input/uinput.html)
- [Linux input event semantics](https://docs.kernel.org/input/event-codes.html)
- [Wayland architecture](https://wayland.freedesktop.org/docs/book/Architecture.html)
- [libinput architecture](https://wayland.freedesktop.org/libinput/doc/latest/architecture.html)
- [systemd udev](https://www.freedesktop.org/software/systemd/man/latest/udev.html)

## Reuse and legal wording

The canonical project follows an independent-implementation/no-artifact-reuse
policy. It does not claim formal clean-room separation or legal immunity.
Project policy forbids importing LapCal code, PCB, CAD, data, weights, or other
protected expression into the canonical implementation.

CC BY-NC-SA 4.0 permits specified noncommercial exercises of licensed rights
subject to its conditions. It does not govern a use that needs no copyright or
similar-rights permission, and it grants no patent or trademark rights. The
idea/expression boundary and any exception depend on facts and jurisdiction.
This audit is not legal advice.

Primary sources:

- [CC BY-NC-SA 4.0 legal code](https://creativecommons.org/licenses/by-nc-sa/4.0/legalcode.en)
- [Creative Commons FAQ](https://creativecommons.org/faq/)
- [17 U.S.C. §102(b)](https://www.copyright.gov/title17/92chap1.html#102)

## Remaining physical unknowns

No paper, source audit, build, simulation, or remote test establishes:

- electrical integrity of the assembled harness;
- capture of every 75 us data-ready pulse;
- sustained one-hand 208 Hz reads and 200 Hz aggregates;
- passive-surface contact observability;
- same-finger Core-28 separability;
- keyboard-to-surface transfer across remounts;
- continuing wearable position after user confirmation, which the selected
  sensors cannot prove;
- two-hand live text accuracy, false-activation rate, or WPM; or
- comfort, durability, power, radio, or product compliance.

The roadmap turns each unknown into a finite go/stop gate. A failed physical
gate is a valid result and must not be replaced with a software fallback or an
instrumented surface under the current product boundary.
