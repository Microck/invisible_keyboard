# Passive-surface invisible keyboard reference architecture

## Status

This document is the contract for the first independent implementation under
the project's no-artifact-reuse policy. It replaces the feasibility dossier's
optical product recommendation after the user fixed the product boundary on
2026-08-09.

The product uses wearable sensors and a passive contact surface. The surface
contains no sensors, emitters, marks, projection, or electronics. Cameras and
instrumented surfaces may supply laboratory truth, but they are not part of the
product input path.

Every claim in this document uses one of these evidence labels:

- **Source-verified:** directly established by an inspected source artifact.
- **Statically verified:** established from code, a schema, or a calculation
  without executing physical hardware.
- **Replay-tested:** established by deterministic recorded or generated input.
- **Simulated:** established only inside an explicit signal model.
- **Hardware-dependent:** requires a physical prototype and measurement.

Simulation can validate software contracts and failure handling. It cannot
establish whether real finger motion contains enough information to identify a
key. The [source integrity audit](../research/source-integrity-audit.md)
records the primary-source checks that changed the acquisition and licensing
contracts.

## Product contract

### Required product

The first product is a personal, calibrated wearable that lets a user enter
text by tapping an ordinary rigid surface with Core-28 enabled on a 34-position
physical grid. The letter rows resemble split US QWERTY, but the project defines
`B` as left-index, Backspace as left-thumb, and Space as right-thumb input. The
two outer-thumb positions remain reserved.

The first implementation must satisfy all of these constraints:

- The contact surface is passive and unmarked.
- All product sensing travels with the user.
- Each hand is independently wearable and locally timestamped.
- Raw sensing and decoding remain local.
- The decoder emits exact key events without a language model.
- Personal calibration is allowed for the first prototype.
- A complete don/doff remount is a separate session.
- A saved validation result authorizes model use and replay, not continuing
  wearable position or live output.
- Every live process requires a fresh user attestation that both wearables have
  not moved. The system cannot sense removal, movement, or loosening.
- The first advertised manifest is Core-28: A-Z, Space, and Backspace.
- Unsupported positions produce no key event.
- Core-28 supports taps only. Hold, repeat, modifiers, chords, and rollover are
  unsupported until a later gate explicitly adds them.
- A crash, disconnect, malformed frame, or decoder reset cannot leave a key
  down.

### Explicit non-goals for the first implementation

- Free-air typing without contact
- A camera, headset, radar, microphone array, or instrumented desk in the
  product path
- Cross-user decoding before personal calibration works across remounts
- Autocorrection that hides raw key errors
- BLE before the wired acquisition and output paths pass their fault tests
- Reusing LapCal source, PCB, CAD, data, or model artifacts

### Provisional physical setting

The first target is seated two-hand typing on a rigid desk into a Linux host.
This is a test setting, not a product restriction. Wood, laminate, glass, and
metal become separate held-out surface conditions after the first one-surface
session passes.

The first physical build should target a cash envelope at or below USD 230 for
the one-hand falsification rig and at or below USD 400 for the two-hand wired
rig.
These are provisional engineering caps, not an approved purchase budget.

## What LapCal actually specifies

The local reference is `research/LapCal` at audited main commit
`8638759168d84d32fa50ae461e053055a01c3dff`. The full evidence and defect
record is in the [LapCal evidence audit](../research/lapcal-evidence-audit.md).

### Hardware topology

**Source-verified design intent:** LapCal's hand-specific configuration and
firmware paths describe one mainboard for each hand. This is topology and
configuration evidence, not a demonstrated synchronized two-hand system. Each
intended hand has six MPU6050-class six-axis IMUs:

1. Thumb sensor
2. Index-finger sensor
3. Middle-finger sensor
4. Ring-finger sensor
5. Pinky-finger sensor
6. Mainboard or dorsal-hand reference sensor

The mainboard uses an ESP32-WROOM-32E, a TCA9548A I2C mux, an onboard MPU6050,
five external sensor connectors, USB-C through a CH340C, microSD, battery power
circuits, and the external sensor units. The audited PCB dimensions are about
12.45 x 19.7 mm per sensor board and 52 x 39 mm for the mainboard.

The local audit preserves the detailed evidence paths. Durable primary links
pin the project goal and finger subsets in the
[LapCal README](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org)
and the hardware sources at audited commit
`8638759168d84d32fa50ae461e053055a01c3dff`.

### Data path

**Source-verified:** LapCal selects the six IMUs sequentially through the mux.
For each sensor it reads the MPU6050 DMP FIFO and produces:

- linear acceleration X, Y, and Z as signed 16-bit values; and
- fused yaw, pitch, and roll scaled by 100 into fields named `gx`, `gy`, and
  `gz`.

Those `g*` fields are not raw angular velocity. The independent implementation
uses unambiguous axis names and retains raw accelerometer and gyroscope
readings before any optional fusion.

The main loop sends one per-hand aggregate either as formatted serial text or
an intended BLE record. Its timestamp is an epoch-like startup base plus MCU
milliseconds. The two hand devices do not have a demonstrated shared clock.

LapCal's stored six-sensor timing logs imply only about 14.3 to 19.1 aggregate
scans per second. This does not meet the new implementation's provisional
5 ms sample-period target.

### Transport defects that become regression requirements

**Statically verified:** the LapCal BLE implementation has two independent
memory/protocol defects:

- It allocates storage for six readings but loops over sensor indexes 0 through
  6, reading a seventh element and writing beyond the 80-byte packet.
- It intends an 80-byte packet but transmits only 76 bytes.

The left-hand calibration table also duplicates the middle-finger offsets for
the ring finger. The independent protocol tests must reject truncation, extra
sensors, ambiguous frame length, and unknown calibration identity.

### Exact 34-slot topology and packed ordering

**Source-verified:** LapCal's keyboard-state format assigns five bytes to each
hand:

- one byte each for pinky, ring, and middle columns;
- one byte containing two three-position index-finger columns; and
- one byte for the thumb group.

The visualizer renders five three-position finger columns and two thumb
positions per hand. This is 15 + 2 = 17 slots per hand, or 34 total. The
[software format](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Software/README.org),
[visualizer](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Utilities/keyboard-visualization/src/main.rs),
and
[grouping diagram](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Documentation/diagrams/keyboard-state-format-grouping.svg)
also establish the packed group and bit ordering.

The reference therefore establishes this slot topology, not metric key
coordinates, dimensions, or a complete logical map:

```text
left hand                              right hand

P0  R0  M0  I0  I3       |       I3  I0  M0  R0  P0
P1  R1  M1  I1  I4       |       I4  I1  M1  R1  P1
P2  R2  M2  I2  I5       |       I5  I2  M2  R2  P2
              T0 T1       |       T1 T0
```

`P`, `R`, `M`, `I`, and `T` mean pinky, ring, middle, index, and thumb. The
numbers are positions within that sensor's output group, not character codes.

**Source-verified absence:** the inspected LapCal repository does not define a
complete logical character mapping for these 34 positions. It references a
34-key Sweep keyboard and records physical keyboard-state bits. Any claim that
LapCal's 34 positions mean a particular set of letters or control keys would be
an inference, not a recovered fact.

## Core-28 on the canonical 34-position physical grid

This project supplies the logical mapping that the reference lacks. It keeps a
US-QWERTY-shaped letter grid, then explicitly defines the otherwise ambiguous
`B` and thumb ownership. These meanings are project-authored; LapCal establishes
only the 34-slot topology and packed ordering.

```text
Q  W  E  R  T       Y  U  I  O  P
A  S  D  F  G       H  J  K  L  [reserved:semicolon]
Z  X  C  V  B       N  M  [reserved:comma] [reserved:period] [reserved:slash]
       [reserved:left-outer] Backspace   Space [reserved:right-outer]
```

### Core-28 enabled positions

| Hand | Sensor | Enabled positions |
| --- | --- | --- |
| Left | Pinky | Q, A, Z |
| Left | Ring | W, S, X |
| Left | Middle | E, D, C |
| Left | Index | R, F, V, T, G, B |
| Left | Thumb | Backspace |
| Right | Index | Y, H, N, U, J, M |
| Right | Middle | I, K |
| Right | Ring | O, L |
| Right | Pinky | P |
| Right | Thumb | Space |

The decoder must treat these six positions as reserved, not as aliases:

- Semicolon
- Comma
- Period
- Slash
- Left outer thumb
- Right outer thumb

An activation classified as reserved is an abstention. It must not become the
nearest enabled key.

### Expansion path

The expansion path adds semantics without changing the 34-position geometry:

1. **Core-28:** A-Z, Space, and Backspace as discrete taps.
2. **Core-30:** enable left outer thumb as Shift and right outer thumb as Enter
   only after down/up and hold timing pass.
3. **Core-34:** enable Semicolon, Comma, Period, and Slash after each class meets
   the same held-out key-identity gate.
4. **Layer 1:** numbers and common symbols behind a qualified thumb hold.
5. **Control layer:** arrows, Escape, Tab, Control, Alt, and platform shortcuts
   only after modifier, chord, repeat, rollover, and all-up recovery tests pass.

No layer may silently replace or reinterpret an existing Core position.

## Independent hardware architecture

The hard reference is LapCal's topology, not its implementation artifacts:

```text
five finger IMUs ----\
                      -> per-hand controller -> wired framed stream --\
dorsal reference IMU -/                                             |
                                                                    v
five finger IMUs ----\                                      Linux recorder
                      -> per-hand controller -> wired framed stream -> decoder
dorsal reference IMU -/                                             -> key state
                                                                    -> uinput
```

### Decisions retained from LapCal

- One directly identified sensor per finger
- One dorsal reference sensor per hand
- Per-hand local acquisition
- Finger identity derived from the physical sensor channel
- Position classification partitioned by finger
- Small wearable sensor boards connected to a hand controller

### Decisions changed from LapCal

- Use raw timestamped acceleration and angular velocity, not only DMP-derived
  linear acceleration and orientation.
- Configure all six sensors at 208 Hz and emit per-hand aggregates on a
  provisional 200 Hz grid through declared latest-sample decimation.
- Give every aggregate a monotonic microsecond timestamp and sequence number.
- Use a fixed-length, versioned, checksummed binary frame.
- Account for every sent, received, duplicate, late, corrupt, and missing
  sequence number.
- Use wired USB serial for the first acquisition rig.
- Synchronize both hand clocks to host monotonic time and retain residuals.
- Separate immutable recording from calibration, decoding, and OS output.
- Add BLE only after wired acquisition passes Gate 0A.

The 200 Hz aggregate rate is a provisional implementation target.
**Statically verified:** it gives a 5 ms frame period. That period and the
inter-sensor timing limit are separate from the stricter 2 ms p95
controller-to-host fit residual. **Hardware-dependent:** actual bus
utilization, interrupt capture, jitter, direct-register delivery, power, and
delivered two-hand rate remain unproved.

The live clock path uses the newest complete 250 ms device-time suffix, a 500
ms initial no-fit deadline, and at most 250 ms of last-good-fit holdover after a
quality-only failure. It never removes an interior pair from the chosen suffix.
The current mapping must not regress and must be within 20 ms of its receipt
before output can arm. Watchdog, identity, timestamp, or source faults clear
both hands' clock state. Full-session evidence remains a separate strict fit
over every decoded raw receipt pair; a recent operational fit is never labeled
as a complete-session epoch.

The two USB reader threads timestamp each chunk immediately after `read`, then
publish it to one merger. The default merger allows at most 50 ms of
cross-thread publication reorder while retaining the original receipt time.
An arrival behind that watermark is a hard transport fault; it is never
retimestamped. The bound remains below the 100 ms connected-source silence
timeout, but consumes half of the provisional 100 ms p95 end-to-end latency
budget. Gate 0S must measure the selected host under load and either justify
this bound or lower it before product latency claims.

The live mapped-time merger must not overtake the receipt merger. Each admitted
update carries the transport's committed watermark, which depends on its actual
configuration rather than a copied 50 ms constant. Live commits that watermark
only after processing every released candidate. Per-candidate flushing stops at
the candidate's receipt minus the permitted 20 ms mapped/receipt delta.
Every output-producing release samples actual consumer time after raw evidence
I/O, including bootstrap backlog and clock arming. The same non-emitting health
gate precedes candidate processing and tick-time flushing. Watchdog progress is
the latest successfully identity- and integrity-checked raw receipt per hand,
not the older candidates released by the receipt merger; validation runs once
at ingress. Candidate-free bytes never renew it. Stale warmup remains
nonauthorizing, even when both clock fits are ready: the gate withholds first
arming while clock processing retains its existing primary-fault policy. Once
armed, an expired hand terminally revokes before any candidate or backlog can
emit. Admission checks receipt-time health before renewing valid-frame progress;
a later receipt cannot revive a terminal instance. Tick-time flushing stops at
the committed upstream watermark minus the same delta. Consumer time alone
cannot release mapped backlog. The 20 ms
mapping bound and mapped merge allowance currently have equal values, but the
safety calculation uses the mapping bound. The default two-stage path combines
a 50 ms receipt-reorder allowance with a 20 ms mapped-reorder allowance.

Device transport, calibrated replay, and calibrated live merging each retain
at most 64 pending records. The normal two-hand 200 Hz stream retains about
eight frames inside the live 20 ms merge window. A 65th retained record is a
typed capacity fault. Replay forces neutral before returning; live clears
pending work and clock evidence, forces neutral, terminally revokes that
wear-session instance, and cannot rearm it. The first terminal cause remains
authoritative even if recorder finalization, cleanup, or output later reports a
second error.

### Selected first topology

The planned first one-hand rig uses a plain Seeed Studio XIAO nRF52840 Plus, an
Adafruit TCA9548A mux, and six identical Adafruit LSM6DS3TR-C breakouts. Each
sensor has its own selectable mux branch and data-ready GPIO. Deselecting a TCA
branch disconnects it from the upstream bus; the mux does not provide galvanic
isolation. Firmware configures the LSM6DS3TR-C for pulsed DRDY, timestamps each
observed rising edge, requires both accelerometer and gyro new-data status,
reads the direct motion registers, and retains the latest complete sample per
sensor. After I2C completes, a worker locks the sample cache and then the
sensor state, raises D9, stores and latches one cache slot, holds D9 for at
least 2 us, attempts D9 low, and then clears the pending state while still
holding the sensor-state lock. A successful D9 falling edge is the cache
eligibility boundary. A same-sensor physical INT1 before that fall invalidates
the Gate 0A capture.

Rev A does not yet authorize the XIAO 3V3 pin to power that fanout. The
controller-to-fanout bridge `PWR_LINK` remains open through split-rail Gate B.
Seeed's [official Plus v1.1 KiCad source](https://files.seeedstudio.com/wiki/XIAO-BLE/Seeed_Studio_XIAO_nRF52840_Plus.zip)
identifies a 3.3 V TLV733P, and the
[TI TLV733P datasheet](https://www.ti.com/product/TLV733P) rates that regulator
for 300 mA. Neither source states current available from the board's `3V3_OUT`
after board demand, voltage drop, and thermal limits. A
regulator headline rating or measured fanout current cannot fill that missing
board-level guarantee. Gate B instead qualifies a fixed, current-limited total
USB-input envelope and measured thermal behavior for a supervised prototype.
It does not claim manufacturer certification. Keep the qualified power path
through subsequent trials. The exact engineering limits, required instruments,
source ledger, and reset-aware power order are in the
[Rev-A assembly baseline](rev-a-assembly-baseline.md#power-pull-ups-and-capacitors).

At each independent 5,000 us aggregate boundary, the main path locks the same
cache, raises D8, copies and consumes all six latest slots, holds D8 for at
least 2 us and less than 20 us, lowers it, and unlocks. D8 rising is the
selection boundary and the copied array stays immutable during encoding. D8
and D9 never overlap. Before
sensor I/O or interrupts, firmware emits `D8 high >=20 us, both low >=20 us,
D9 high >=20 us, both low >=20 us`. This prefix is an anchor only as the first
activity with no INT1 or I2C event. The first later normal D8 maps to sequence
0; every later normal D8 advances the sequence once regardless of DTR, and a
reconnect never resets it.

The 208 Hz sensors intentionally feed the 200 Hz aggregate grid through
latest-sample decimation. Nominally, 8 reads per second per sensor are replaced
before selection. This expected decimation is not an acquisition overrun. Each
selected sample retains its source-edge timestamp, so skipped samples remain
visible as longer edge intervals. Acquisition overrun is reserved for
unexpected pending-read collisions, read or coherence errors, missing fresh
data, deadline misses, and transport faults. Direct output registers do not
prove that every internal sensor conversion was retained. Gate 0A must account
for the full analyzer capture, including pulsed INT1 edges, reads, collisions,
cache-commit markers, snapshot markers, selected samples, and deliberate
decimations. Without that pulse evidence it cannot make an every-conversion
claim. These register and DRDY semantics come from the primary
[LSM6DS3TR-C datasheet](https://www.st.com/resource/en/datasheet/lsm6ds3tr-c.pdf).

Gate 0A's decisive input is the native little-endian `u16` capture, not an
external evidence JSON file, decoded NDJSON, or a human timing total. One
DSLogic Plus records ten channels: bits 0 through 5 for INT1, bit 6 for D8,
bit 7 for D9, bit 8 for SDA, and bit 9 for SCL. The full run requests
7,220,000,000 words at 2 MHz, produces 14,440,000,000 bytes, and scores at
least 3,601 seconds. The 20 MHz 10-second preflight and 2 MHz 10-minute soak are
non-decisive. Build, finalization, hashing, and scoring run in a Crabbox; only
physical capture runs on the analyzer host.

Gate 0A ends through one deterministic hard cut on that same controller byte
stream. IKS3 records one outbound `IKQ2` request, one clean final SensorFrame
`N`, and one matching inbound `IKR2` receipt. Final D8 closes admission on an
ordinary scheduled boundary. Firmware then performs exactly 52 shutdown
transactions and 1,422 SCL rises, toggles D8 and D9 together for the one
terminal marker, and emits no application byte after the receipt. The analyzer
must retain at least five seconds of idle samples after marker fall. The host
guards the TTY through analyzer EOF plus 100 ms before writing `END3`.

The capture wrapper owns sigrok, the Bazel-built recorder, readiness pipe, stop
pipe, and analyzer-success guard. Stdout, Ctrl-C, reset, power-off, manual
trimming, IKS2, and a receipt sidecar are not success paths. The finalizer runs
only in a Crabbox, hashes raw and IKS3, and requires the scorer bytes to equal
the wrapper-published recorder bytes. The exact contract is in
[Gate 0A deterministic quiesce](gate0a-quiesce-contract.md).

Follow the capture guide's [Canonical artifacts](gate0a-capture-stack.md#canonical-artifacts),
[Box validation record](gate0a-capture-stack.md#box-validation-record),
[Physical preflight sequence](gate0a-capture-stack.md#physical-preflight-sequence),
[Physical-host capture](gate0a-capture-stack.md#physical-host-capture),
[Crabbox finalization](gate0a-capture-stack.md#crabbox-finalization), and
[GPL and firmware provenance](gate0a-capture-stack.md#gpl-and-firmware-provenance)
sections. The firmware bundle has fixed byte identities but no reproducible
mapping from the available source snapshots, so redistribution remains a legal
and supply-chain risk. The analyzer establishes source order, not host receipt
time: USB and host-kernel backlog may delay a read after D8, and the trace does
not observe DTR or the kernel read boundary.

Six free-running 208 Hz sensors may span almost 4.81 ms, so the frame retains
each reference-edge offset and the provisional decoder ceiling is 5 ms. The
co-read gyro conversion is not separately timestamped; Gate 0A must measure or
ablate that within-sensor phase before any simultaneity claim.

The exact BOM, pin map, register configuration, bus budget, assembly rules,
gates, and fallbacks are in the
[first prototype roadmap](first-prototype-roadmap.md).

## Host language decision

The canonical host stack is Rust.

Rust is the best fit because the first stack needs:

- exact fixed-width binary parsing;
- explicit error handling for corrupt or missing frames;
- deterministic replay and bounded state machines;
- low-latency Linux input integration;
- memory safety around untrusted sensor transport; and
- a Bazel-owned host graph that also tests the portable C firmware boundary.

Python remains an offline analysis and model-training option after real data
exists. It does not own the wire protocol, session truth, key-state machine, or
live output path. TypeScript would add no useful runtime or embedded advantage
for this stack. C or C++ remains limited to a future firmware adapter when the
selected controller's supported toolchain requires it.

## Deep module seams

The implementation uses narrow modules around one canonical path.

### Frame codec

Interface:

```text
encode(SensorFrame) -> bytes
decode(bytes) -> SensorFrame | FrameError
```

It owns versioning, fixed lengths, integer byte order, checksum validation, and
sensor-count validation.

### Stream transport

Interface:

```text
push(arbitrary_bytes) -> zero or more bounded frame candidates
finish() -> optional unterminated candidate
```

It owns COBS framing, chunk independence, delimiter recovery, and bounded
oversized/malformed evidence.

### Session recorder

Interface:

```text
append(candidate_kind, captured_bytes, original_length, host_time)
finish() -> SessionSummary
audit_stream(reader) -> SessionAudit
audit_and_fit_clocks(reader, manifest) -> SessionAudit + exact ClockEpochSet
```

It owns the hard-cut IKS3/REC3/END3 container, append-only storage,
sent/received accounting, duplicate and reorder detection, clock-fit inputs,
transport-fault provenance, metadata hashes, and strict
corruption/truncation detection. Record kinds distinguish SensorFrames,
malformed/oversized/unterminated transport evidence, one outbound `IKQ2`, and
one inbound `IKR2`. Only the Gate 0A command admits control records. Every
calibration, neutral, ordinary-recording, replay, and live consumer rejects
them. No IKS2 reader or writer exists.

Finite setup and Gate 0A loaders use their acquisition-derived byte caps before
allocation. Generic verification, clock fitting, and live finalization stream
without a short duration cap. The exact clock fitter scans the raw session
once, spools only timestamp pairs, and uses bounded histograms to reproduce the
in-memory p50, p95, p99, and maximum residuals exactly.

A clock report covers exactly the manifest's declared hands. A single-hand
capture has one left or right epoch; a two-hand capture has left then right.
Binding checks shape and the exact session, controller, and boot identities;
it does not establish offline clock authority. Before capture, neutral, fitting,
held-out, or calibrated replay admission, `qualify_session` recomputes the exact
bounded fit from raw receipt times and requires every supplied field to match,
including floating-point bits. Nested consumers reuse one immutable
`QualifiedOfflineClocks` value. Causal operational fits remain separate from
this whole-session qualification.

`SensorFrame.device_time_us` and every `sample_time_offset_us` always remain in
raw device microseconds. Identity-bound offline admission checks the complete
per-hand stream for acquisition quality, consecutive sequences, strictly
increasing aggregate time, and strictly increasing representable sensor times
before truth-window selection or host-time scheduling. A repeated aggregate is
invalid even when every sensor offset advances. Recording still retains the
original bad bytes. Raw-integrity admission errors name the hand, record
ordinal, and fault. Calibrated replay makes its canonical all-up attempt before
returning a raw admission failure.

### Decoder

Interface:

```text
calibrate_split(training_windows, threshold_windows) -> Calibration
decode(frame_window, calibration) -> Decision
```

It owns contact intent, per-finger position classification, confidence,
abstention, and causal window scoring. It cannot access future samples or a
language model.

### Causal pipeline

Interface:

```text
push(SensorFrame) -> zero or one debounced Decision | PipelineError
reset() -> empty causal state
```

It owns the rolling window, fixed decision delay, one-result-per-tap
suppression, idle re-arm, and stream reset on structural timing failure.
The pipeline can retain a classification when later windows become idle, but
its emitted decision time is always the current commit frame's raw device
time. Host-time ordering belongs to the calibrated runtime output seam.

### Calibrated runtime time domains

Interface:

```text
push_frame(raw_frame: SensorFrame, output_time_us: u64) -> zero or one RuntimeStep | RuntimeError
```

The causal pipeline consumes raw frame and sensor times. Its commit decision
time remains the current raw aggregate time. Live and calibrated replay keep
already-qualified mapped time beside the raw frame for merge ordering, then
supply that mapped time in microseconds to the runtime. The runtime projects
every emitted decision at the output seam and tracks safety-neutralization
time in the same output domain. It does not map or scale sample offsets.

Raw byte and candidate inputs supply the decoded frame's raw aggregate time as
output time, preserving their raw-timeline contract. Continuous held-out
validation also passes raw frames to its pipelines while retaining mapped time
for scheduling and key-state transitions.

### Key-state machine

Interface:

```text
apply(Decision) -> zero or more KeyEvents
fault(reason) -> all-up KeyEvents
```

It owns supported usages, down/up order, duplicate suppression, reserved-key
rejection, disconnect recovery, and the invariant that no key remains down.

### Output adapter

Interface:

```text
emit(KeyEvent) -> OutputReceipt
release_all() -> confirmed all-up | OutputError
```

The first adapters are an in-memory recorder and Linux `uinput`. Creating the
virtual device requires write permission on `/dev/uinput`; ownership, group
mode, ACLs, and udev policy determine access, not a kernel rule that the process
must run as root. The
[kernel uinput guide](https://docs.kernel.org/input/uinput.html) defines the
interface. Tests cross the same seam. USB HID and BLE HOGP are later adapters,
not parallel decoder paths.

An empty software key set is not proof that the adapter emitted every release.
Terminal reports keep the first operational cause, retain an independent typed
neutralization failure, and set `output_neutrality_confirmed` only after the
adapter confirms all-up. User-facing neutral success copy depends on that
boolean.

### Live termination and durable evidence

Live termination has two separate milestones. First, the canonical output
owner attempts all-up synchronously when it observes a signal or operational
fault. Gate 0S measures that adapter-to-independent-evdev observation against
250 ms. A status line is not the safety oracle. Scheduler, mutex, and kernel
progress make this an empirical Linux gate, not a hard-real-time theorem.

Second, the recorder atomically closes its bounded reader mailbox. Every chunk
admitted before that cutoff is drained into evidence without reaching the
decoder or output again. A successful FIFO write proves kernel acceptance, not
mailbox admission, so the durable session is the exact gap-free admitted
prefix. The first terminal cause is immutable. A later signal, reader EOF,
status error, or evidence error cannot relabel it.

Finalization writes the checksummed `END3` footer, syncs the session file and
its parent directory, runs the strict streaming audit and exact clock fit, and
writes a v2 live clock report bound to both the runtime-manifest SHA-256 and the
SHA-256 of the complete session bytes. It then syncs that report and its parent
directory. Only after those steps can the CLI publish
`evidence_finalization=durable`. If that status write fails, the returned error
states that the bundle is durable but status publication failed.

Footer presence is not durable success. Pre-finish protocol and guard failures
leave no `END3`, but a later session-file or parent-directory sync failure or a
strict post-footer audit I/O failure can leave `END3` bytes in an unpublished
partial. The operation still exits nonzero. Only the complete sync, audit,
report, and publication sequence establishes success.

There is no universal wall-clock bound for evidence finalization. Session size,
storage behavior, and exact clock fitting determine its duration. The current
parent-directory sync implementation and syscall proof apply to Unix, with a
permanent Linux `strace` oracle. Other operating systems require their own
durability implementation and proof before receiving the same claim. The live
recording adapter retains only receipt ordinals, and session audit and clock
fit remain streaming, so run duration does not create an in-memory event log.

## Decoder baseline

The first baseline is deliberately small and causal:

1. Subtract or otherwise compare the acting finger's motion with its dorsal
   hand reference.
2. Detect a contact-like impulse and select one acting sensor.
3. Extract a fixed pre-event and causal event window.
4. Classify only among positions owned by that sensor.
5. Reject a reserved position or confidence below threshold.
6. Emit one tap decision. Core-28 converts it to one down/up pair.

The first trainable classifier is a regularized nearest-centroid baseline over
documented window features. It is not expected to be the final model. It is
valuable because its calibration, distance, rejection, and confusion behavior
are inspectable. A temporal neural model is authorized only if real
session-held-out data shows a specific baseline failure that additional time
structure could address.

The runtime has no language model. An optional text correction layer can be
evaluated later, but raw events and raw text remain the product evidence.

## Guided personal calibration

The first setup uses a regular physical keyboard to supply selected physical key
events while the user wears the sensors. It then moves the keyboard away and
runs a prompted passive-surface transfer plus locked validation. Keyboard warm
start is a non-admission signal precheck. It cannot approve invisible mode
because switch motion differs from desk contact. `B`, Space, Backspace, and
both outer-thumb positions are passive-only.

The version 1 keyboard corpus has exactly three prompt kinds: natural,
coverage, and reserved. It has no correction or practice drill, and the UI does
not show unused corpus targets. Adaptive selection sums each candidate prompt's
per-position `count_shortfall + separated_block_shortfall`; ties rotate by
attempt number. This changes collection order only. It supplies no decoder
language model or admission score.

The user identifies one physical label keyboard, then explicitly confirms or
rejects the displayed US-QWERTY physical rows. The service binds the frozen
copy hash, prompt ID, dedicated instruction revision, and accept time before it
admits neutral capture. It then derives a gravity-referenced neutral-orientation
and relative-motion baseline from a three-second rest block. It does not claim
absolute hand pose or key coordinates, and it does not ask for keyboard angle,
switch travel, hand measurements, or manual sensor angles.

For each passive target, the browser acknowledgment opens a provisional
1.5-second response gate; it does not label contact. A sensor-derived causal
contact observation determines whether a sample exists. Both hands are
inspected. No contact, the wrong hand or finger, and multiple contacts are
explicit rejected outcomes. The retained target segment is 1.8 seconds.
The receipt proves the service's publication and acknowledgment record, not
that a human saw the cue or that the physical mount and contact were authentic.
Physical studies need an honest operator and independent truth for those facts.

The collection trigger is frozen before passive fitting and remains distinct
from the final decoder threshold. Passive centroid evidence and final-threshold
selection evidence are disjoint, and held-out validation can mutate neither.
Ordinary-motion exposure counts only measured motion-rich, non-contact,
non-saturated windows. Setup is guided, finite, and capped at 1,200 seconds. It
never promises a 100 percent calibration result.

The persisted successful result is `CalibrationPhase::ValidationPassed`,
serialized as `validation-passed`, with
`CalibrationSnapshot.heldout_validation_passed=true`. Failure uses
`CalibrationPhase::CalibrationFailed`, serialized as `calibration-failed`.
`HeldOutValidation` authorizes the model and replay only; structural failures
use `HeldOutValidationError`, and the artifact must set
`requires_wear_session_confirmation=true`. Live output requires a fresh
per-process `WearSessionConfirmation` based on the user's attestation that both
wearables have not moved. The system cannot detect removal, movement, or
loosening. Any source, watchdog, identity, clock, output, or process fault
terminally revokes that live instance. Each restart recomputes the saved
held-out result from its artifact-bound plan, schema-v9 facts journal, clock
fits, and raw session, compares the cached report, and requires a new
confirmation; it cannot silently rearm. This happens before opening a controller
or `uinput` output. If either wearable moved, the user must revalidate before
confirming.

Every completed candidate passes live fault admission before the transport
receipt merger or the mapped-frame queue can defer it. Malformed transport and
raw-frame decode errors revoke the instance with their original first cause.
Every identity-bound frame also passes acquisition and continuity checks before
clock warmup, including post-arm partial windows. Missing sensors,
unsynchronized or overrun flags, excessive sample skew, sequence gaps, and
repeated or regressing raw aggregate or sensor times revoke immediately.
Unrepresentable raw sensor times also revoke. An advancing sample offset cannot
renew repeated aggregate time. Later clean frames cannot restore authority.

`LiveCalibratedRuntime::admit_transport_event` uses one fresh `DeviceTransport`
exclusively for that live instance, including its bootstrap backlog. Its
single-use update exposes raw evidence for recording; only
`release_transport_update(update, consumer_time_ns)` can then consume it for
clock and decoder work. The consumer samples that time after appending evidence.
The shared health gate precedes arming and every release path. Integrity faults
neutralize output and close external admission before evidence I/O. A committed
faulty chunk is retained once, followed by transport-held and
mailbox-admitted backlog and partial suffixes. Terminal evidence-only draining
never returns candidates to live output.

The valid-frame watchdog advances after every receive iteration, including
chunks that produce no candidate. Empty delimiters or partial-frame byte
traffic cannot keep a wear session active after valid frames stop.

The front-end reports hardware health, per-position coverage, and locked
validation separately. It has explicit time/example caps and cannot force a
passed validation artifact. See the
[calibration experience contract](calibration-experience.md).

### Calibration authority and artifact roles

| Object | Role |
| --- | --- |
| Canonical fitting or held-out surface plan | Owns the expected position or null meaning, prompt order, purpose, and, for held-out use, locked-artifact binding. |
| Schema-v9 capture journal | Stores observed facts, the complete display-acknowledgment chain, and plan identity beside the raw session. Admission reconstructs every canonical presentation time plus cumulative render delay, rejects missing, extra, reordered, compressed, shifted, relabeled, wrong-purpose, or wrong-artifact facts, and reserves every source record in each 16-frame truth window. The engine independently rejects same-hand truth-window intersections. |
| Acquisition manifest, clock fits, raw session, and fitting neutral report | Canonical consistency inputs used with the journal to derive observations. |
| Calibration evidence JSON | Privacy-minimized diagnostic output derived from the plan, journal, clocks, manifest, and raw session. It is not accepted as fitting or validation authority. |
| Schema-v4 model artifact | Locks the decoder and records both its fitting-journal hash and derived fitting-evidence hash. The latter fingerprints an output; it does not create another admission path. |
| Validation report JSON | Inspectable derived cache. Runtime recomputes it from canonical held-out inputs and requires equality before it can mint `HeldOutValidation`. |
| `HeldOutValidation` | Opaque model/replay admission created only by successful rederivation. It does not prove wearable continuity or authorize live output. |
| `WearSessionConfirmation` | Fresh per-process user attestation required separately for live output. Sensors cannot verify its claim. |

## Hardware-independent proof contract

The repository can establish these facts without a physical prototype:

- Every Core-28 usage has exactly one physical position.
- Reserved positions never alias enabled positions.
- SensorFrames, `IKQ2`, and `IKR2` round-trip exactly and reject corrupt
  lengths, versions, checksums, reserved fields, sensor counts, hand IDs,
  identities, sessions, and timestamps.
- IKS3 strictly distinguishes frame, invalid-candidate, outbound-request, and
  inbound-receipt records; requires `END3`; rejects IKS2; and makes every
  non-Gate0A consumer reject control records.
- Sequence gaps, duplicates, and reordering are visible.
- Every admitted hand's raw aggregate time and reconstructed raw sensor times
  strictly advance, including outside selected calibration truth windows.
- Valid clock drift changes host-time scheduling and output timestamps, never
  frame device time or sample offsets.
- Every raw sensor vector is transformed by its manifest signed permutation
  exactly once after decode and identity binding, before neutral, fitting,
  held-out, replay, or live decoding.
- Session replay is deterministic.
- Strict audit, hashing, accounting, and complete-session clock fitting stream
  with memory independent of session duration. Finite setup and Gate 0A files
  fail before allocation when their exact acquisition-derived cap is exceeded.
- The decoder uses only current and past frames.
- The rolling pipeline emits one result per simulated tap and re-arms only
  after idle evidence.
- Calibration rejects missing or invalid sensors, clock-unsynced samples,
  acquisition-overrun samples, and excessive inter-sensor timing skew. It does
  not reject the declared 208-to-200 latest-sample decimation; selected source
  timestamps still have to meet the skew rule.
- A selected Linux `EV_KEY` device, acquisition manifest, clock fits, raw sensor
session, canonical surface plan, and schema-v9 facts journal deterministically
  produce privacy-minimized diagnostic evidence. Wrong keyboards, repeats,
  overlap, unmatched events, reused window ends, and facts that do not match the
  complete plan do not enter fitting coverage.
- Browser acknowledgment opens the passive response gate but never supplies
  contact truth. A causal sensor observation accepts exactly one expected hand
  and finger or records no, wrong, or multiple contact as a rejection.
- Keyboard warm start is a non-admission precheck. Passive centroid fitting,
  final-threshold selection, and held-out validation use disjoint passive
  evidence. The frozen collection trigger is not the final decoder threshold.
  A locked artifact binds the manifest, prompt corpus, source session, source
  domains, training policy, clock fits, mount IDs, keyboard identity, coverage,
  and decoder constants.
- Held-out validation must use a different session and an artifact-bound plan.
  It covers all 34 physical positions plus measured motion-rich null exposure
  and cannot mutate the collection trigger, final threshold, or locked artifact.
- The guided calibration coordinator deterministically covers keyboard and
  passive-surface positions, separates validation blocks, locks artifacts, and
  terminates on plateau, example, or time caps.
- One journal-bound replay validates immutable fitting and held-out plans,
  journals, clocks, and raw sessions, drives that coordinator, and produces the
  model, diagnostic evidence, report cache, and final setup state without
  relabeling replay as hardware evidence.
- Synthetic remount, drift, noise, dropout, packet-loss, and ordinary-motion
  cases produce reproducible scorecards.
- The calibrated replay and causal live paths merge both hands through one
  fail-neutral key-state machine. Online clocks must warm before output and
  reset on watchdog or disconnect.
- Every fault path clears the software key state and attempts adapter all-up.
  A failed all-up remains visible and cannot produce confirmed-neutral copy.
- Deterministic event replay produces the expected Linux key-event sequence.
- The Gate 0A software lifecycle closes on one clean final D8 boundary,
  reserves terminal transport order, executes the exact 52-operation shutdown
  model, encodes one simultaneous marker request, records one matching receipt,
  and refuses publication after any supervised child or guard failure.

The repository cannot establish these facts without hardware:

- A real tap is observable at the required rate.
- Same-finger positions are separable across surfaces and remounts.
- The dorsal reference removes enough shared hand motion.
- Pulsed DRDY edges are captured and the provisional 200 Hz aggregate rate is
  delivered by the selected electronics.
- Final D8 physically closes admission, the shutdown produces exactly 1,422
  observable SCL rises, D8/D9 change in one analyzer sample, and the hardware
  remains idle through the finite tail.
- Two hand clocks can meet the required residual under load.
- Calibration completes within the target time.
- Real false activations, WPM, errors, comfort, thermal behavior, battery life,
  radio performance, and durability meet their gates.

These are physical-prototype gates, not missing software features.

## Physical feasibility dispositions

The [physical study protocol](physical-study-protocol.md) is the normative
operator and scoring contract for Gate 0B, Gate 1, and Gate 2. It freezes a
roster and participant-disjoint splits before collection, binds independent
truth and observation artifacts row by row, gives each held-out participant
equal weight, and reports `go`, `inconclusive`, or `no-go` without optional
stopping.

- Gate 0B requires both hands, at least five held-out participants, two real
  remounts each, 50 taps per finger in every hand/mount slice, and equal quiet
  and scripted-motion exposure of at least 60 seconds. Miss, minimum finger
  recall, p95/p99 latency, anticipatory events, extras, and false activations
  decide the disposition.
- Gate 1 freezes the contact detector, uses participant-disjoint train,
  validation, and held-out ownership, and adds shared motion, keyboard typing,
  desk tapping, and simultaneous ambiguity. Participant-balanced event error,
  minimum finger recall, macro-F1, latency, extras, and false activations decide
  the disposition.
- Gate 2 freezes the representation and model selection before held-out use,
  then requires at least eight held-out participants, two remounts each, and 50
  intentional events for all 34 physical positions per mount. It evaluates
  participant-balanced event error, recall across all 34 positions, macro-F1
  across the 28 enabled outputs, latency, extras, and zero enabled outputs on
  reserved positions.

For bounded criteria, an unfavorable point is `no-go`; a favorable point whose
declared one-sided confidence boundary crosses the threshold is
`inconclusive`; only a favorable point and boundary produce `go`. The engine
uses per-participant Clopper-Pearson bounds and a fixed 20,000-replicate
participant bootstrap. Software can enforce those calculations and provenance.
It cannot prove that participants, remounts, truth instruments, or physical
contacts match their declarations.

## Independent implementation and no-artifact-reuse rule

LapCal's repository-wide CC BY-NC-SA 4.0 statement, lack of separate
data/model provenance, and lack of patent grant make direct reuse unsuitable
under this project's conservative policy. This is a project reuse decision,
not a claim that the license forbids every noncommercial use or adaptation.
The implementation may use documented public facts and independently written
requirements. It must not copy or adapt LapCal code, PCB files, CAD, datasets,
calibration values, or model weights without a separate review and approval.

The new protocol, code, layout mapping, schematics, CAD, calibration data, and
models must be created independently and carry explicit licenses and source
provenance.

The canonical project is currently private under the root `LICENSE`; it grants
no reuse rights. [source-provenance.md](source-provenance.md) records the
no-artifact-reuse boundary and the ledger required for future data, models,
CAD, and firmware artifacts. The ignored `research/` corpus retains its own
licenses and is excluded from the canonical project license. The project has
not documented the separated teams, access controls, or communication record
needed to claim a formal clean-room process.

## Implemented hardware-independent artifacts

The implementation uses one Bazel build graph:

- `.bazelversion` pins Bazel 9.2.0.
- `MODULE.bazel` pins rules_rust 0.73.0, Rust 1.94.0, and evdev 0.13.2.
- `MODULE.bazel.lock` records the resolved Bzlmod and Crate Universe graph.
- `BUILD.bazel` owns the library, CLI, unit and integration tests, rustfmt, and
  Clippy targets.

Run all hardware-independent checks in a remote Crabbox with:

```text
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test --test_tag_filters=-manual -- //... -//:gate0a-capture-stack-test
box exec --cwd invisible_keyboard BOX_ID env IKB_GATE0A_TEST_FIRMWARE_BUNDLE=/absolute/path/to/pinned-firmware.tar.gz bazelisk --batch test //:gate0a-capture-stack-test
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:rustfmt-test //:clippy-test
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:output-system-required-test --test_strategy=local
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch build //firmware/zephyr:selected-board-firmware
```

The implemented source surface is:

- `src/layout.rs` for the physical and logical manifest
- `src/protocol.rs` for SensorFrame v2 and exact `IKQ2`/`IKR2` control records
- `src/transport.rs` for bounded COBS stream framing
- `src/session.rs` for hard-cut IKS3 immutable recording and replay
- `src/manifest.rs` for strict acquisition and mount identity
- `src/clock.rs` and `src/clock_epoch.rs` for online and session-bound clocks
- `src/labels.rs` for selected Linux keyboard events and one-to-one labels
- `src/calibration.rs` for guided setup state, coverage, validation isolation,
  and finite stopping rules
- `src/calibration_capture.rs` for the plan-bound schema-v9 facts journal and
  sensor-window matching
- `src/calibration_engine.rs` for derived diagnostic evidence, split fitting,
  and held-out validation
- `src/artifact.rs` for the inspectable locked personal model
- `src/validation.rs` for count-level confusion, validation, and wear-session
  confirmation evidence
- `src/physical_study.rs` for frozen Gate 0B, Gate 1, and Gate 2 rosters,
  append-only evidence, participant-balanced confidence rules, and dispositions
- `src/calibration_app.rs` and `web/` for the loopback service and seven-screen
  setup UI
- `src/decoder.rs` for calibration and the causal baseline
- `src/pipeline.rs` for continuous causal segmentation and re-arm
- `src/runtime.rs`, `src/replay.rs`, and `src/live.rs` for the two-hand replay
  and causal transport-to-output paths
- `src/output.rs` for key-state safety and output adapters
- `src/simulation.rs` for explicit synthetic signal and fault models
- `src/evaluation.rs` for the fixed synthetic scorecard
- `src/main.rs` for evidence-bound record, Gate 0A quiesce, calibration,
  `physical-study-plan`, `physical-study-seal`, `physical-study-score`, replay,
  simulate, evaluate, and inject commands
- `firmware/frame-protocol.*` for the portable C byte/transport contract
- `firmware/acquisition.*` for the portable six-sensor aggregate boundary
- `tests/physical-study-contract.rs` and the physical-study cases in
  `tests/cli-contract.rs` for the frozen study and command contracts; other
  `tests/` remain limited to public contracts that need integration-level proof

The implementation must keep one canonical path. Test adapters replace
hardware at real seams; they do not create a second decoder or compatibility
path.
