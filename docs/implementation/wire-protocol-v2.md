# Invisible keyboard wire protocol v2

## Purpose and proof boundary

This is the independently implemented byte contract between each wearable
controller and the Linux host. SensorFrame remains protocol v2. It preserves
six raw accelerometer/gyroscope readings, per-sensor sample offsets, physical
controller identity, boot identity, sequence, and fault state. Gate 0A adds
the direction-specific `IKQ2` quiesce request and `IKR2` receipt on the same
zero-delimited COBS byte stream. It proves that the C encoder, Rust decoder,
source-aware session, and manifest agree. It cannot prove that real hand motion
identifies a key.

Repository sessions use only the hard-cut IKS3 container. The implementation
does not parse v1 frames or IKS2 sessions and has no compatibility fallback.

## Topology and canonical sensor order

The physical prototype uses one independent USB controller stream per hand.
The host binds the selected left device to `Left` and the selected right device
to `Right`; it does not infer source from payload bytes.

Each controller emits exactly six sensors in this order:

| Index | Role |
| ---: | --- |
| 0 | Thumb |
| 1 | Index finger |
| 2 | Middle finger |
| 3 | Ring finger |
| 4 | Pinky finger |
| 5 | Dorsal hand reference |

The acquisition manifest maps each role to its mux channel, mount, wiring,
interrupt pin, and signed proper axis rotation. Firmware emits raw sensor axes.
The host applies the manifest rotation exactly once after frame decoding and
identity binding, before neutral qualification, fitting, held-out evaluation,
replay, or live decoding.

## Exact 160-byte frame

All multibyte integers are little endian. C code writes each field explicitly;
no packed C struct is placed on the wire.

| Offset | Length | Type | Field |
| ---: | ---: | --- | --- |
| 0 | 4 | bytes | ASCII magic `IKB2` |
| 4 | 1 | `u8` | protocol version, exactly `2` |
| 5 | 1 | `u8` | payload hand: `0` left, `1` right |
| 6 | 1 | `u8` | sensor count, exactly `6` |
| 7 | 1 | `u8` | valid-sensor mask, bits 0 through 5 only |
| 8 | 4 | `u32` | status flags |
| 12 | 4 | `u32` | wrapping per-hand sequence |
| 16 | 8 | `u64` | aggregate controller time in microseconds |
| 24 | 32 | bytes | SHA-256 of the manifest controller ID string |
| 56 | 16 | bytes | random nonzero boot ID |
| 72 | 14 | sensor | thumb sample |
| 86 | 14 | sensor | index sample |
| 100 | 14 | sensor | middle sample |
| 114 | 14 | sensor | ring sample |
| 128 | 14 | sensor | pinky sample |
| 142 | 14 | sensor | dorsal sample |
| 156 | 4 | `u32` | IEEE CRC-32 of bytes 0 through 155 |

Each sensor block is:

| Relative offset | Length | Type | Field |
| ---: | ---: | --- | --- |
| 0 | 2 | `i16` | acceleration X |
| 2 | 2 | `i16` | acceleration Y |
| 4 | 2 | `i16` | acceleration Z |
| 6 | 2 | `i16` | angular velocity X |
| 8 | 2 | `i16` | angular velocity Y |
| 10 | 2 | `i16` | angular velocity Z |
| 12 | 2 | `i16` | sample reference time minus aggregate time, microseconds |

The referenced edge time is:

```text
sample_reference_us[i] = device_time_us + sample_time_offset_us[i]
```

An offset that does not fit `i16` clears that sensor's validity bit and sets
acquisition overrun. Firmware must not clamp or wrap it.

## Physical identity and boot admission

The pre-capture template uses schema
`invisible-keyboard-acquisition-template-v4` and an all-zero boot placeholder
for each declared hand. The host reads both selected devices until it has one
valid frame whose:

- physical source matches the declared hand;
- controller hash matches SHA-256 of the declared controller ID; and
- boot ID is nonzero and unique for this boot.

Only then does it replace the placeholders and write an
`invisible-keyboard-acquisition-manifest-v4` locked manifest. Every later frame
must match that exact source, hand, controller hash, and boot ID. A reset or
cross-connected device is a terminal capture fault, not a new epoch spliced
into the session.

Each of the 12 sensor entries binds its canonical index, mux channel, role,
mount ID, wiring ID, interrupt pin, and axis map. A calibration artifact copies
those bindings and the matching-rule hash, so a remount or wiring change cannot
silently reuse a model.

## Status flags

| Bit | Name | Meaning |
| ---: | --- | --- |
| 1 | Clock unsynced | The controller's local monotonic epoch regressed or repeated |
| 2 | Acquisition overrun | A read failed, was stale, missed its deadline, or had an invalid offset |

Bit 0 has no canonical meaning. It and every other unknown bit invalidate a
frame. The direct-register Rev-A path does not enable the sensor FIFO, so it
must not turn `FIFO_STATUS2.OVER_RUN` into a sample-loss claim.

Version 2 does not need a host-to-controller
clock command. Firmware checks its local monotonic clock; the host later fits
controller time to host receipt time. A local regression latches clock-unsynced
until a new boot.

## Sensor cadence and latest-sample decimation

Each IMU is configured for nominal 208 Hz output, block-data update, and the
approximately 75 us pulsed data-ready mode. Each pulse captures one controller
timestamp and schedules a direct 12-byte motion-register read. The FIFO remains
in reset-default bypass mode.

The controller emits frames on an independent 200 Hz grid. Under one lock, it
copies the newest completed reading for all six sensors. A newer 208 Hz reading
can therefore replace an older reading that no aggregate frame selected. This
is deliberate latest-sample decimation, nominally 8 readings/s/sensor or
3.846 percent of produced readings. It does not set acquisition overrun.

The exact selected pulse timestamp remains in `sample_time_offset_us`.
Acquisition overrun is reserved for unexpected faults such as a pulse arriving
while its read is still pending, an I2C failure, no fresh reading at the frame
boundary, an invalid offset, a missed aggregate deadline, or a failed USB
slot. The direct-register protocol alone cannot prove that every internal IMU
conversion produced a captured pulse.

Gate 0A therefore records all six pulsed data-ready lines, I2C, the D9
cache-commit marker, and the later D8 aggregate-snapshot marker. Both markers
use the sample-cache lock and never overlap. After I2C completes, the worker
locks the cache and then the sensor state, raises D9, stores and latches one
cache slot with the captured edge time, holds D9 high for at least 2 us,
attempts D9 low, and then clears the pending state while the sensor-state lock
remains held. A successful D9 falling edge is the cache-commit eligibility
boundary. A same-sensor physical INT1 edge before that fall
invalidates the Gate 0A capture; the contract does not invent a shorter timing
allowance.

At each independent 200 Hz boundary, the aggregate path locks the same cache,
raises D8, copies and consumes all six latest slots, holds D8 high for at least
2 us, lowers D8, and unlocks. D8 rising is the aggregate eligibility boundary.
The copied six-sample array is immutable during frame encoding. Gate 0A must
therefore select, for each sensor, the newest successfully committed eligible
read before that D8 rise. Every observed read is classified exactly once as
selected or deliberately decimated; stale, reused, after-boundary, ambiguous,
or unclassified selections fail.

Before sensor I/O or interrupt enable, firmware emits `D8 high >=20 us, both
low >=20 us, D9 high >=20 us, both low >=20 us`. The analyzer must be armed
before reset or power-on and recognize that anchor only as the first prefix
with no INT1 or I2C activity. Marker width alone cannot create an anchor. The
first later normal D8 snapshot maps to sequence 0, and every later D8 snapshot
advances the sequence once. DTR and host connection state never gate the
aggregate timer or sequence, and reconnect never resets either one. The host
does not monitor DTR as a fault signal. A host-visible source disconnect or
transport fault terminally revokes live output.

The source-side boundary does not prove host delivery time. USB controller and
host-kernel buffers can retain frames after their D8 snapshots, so a later
host receipt can include backlog. The analyzer does not observe DTR or the
kernel read boundary and must not call D8 a delivered-frame marker. The host
session separately retains receipt time and sequence gaps.

The markers are analyzer instrumentation, not surface sensors or runtime
product inputs.

## Gate 0A decisive evidence

The only decisive analyzer input is the native raw little-endian `u16` capture
plus its machine-generated sidecar. The scorer reads every word directly. It
does not accept external evidence JSON, decoded NDJSON, manually decoded I2C,
or human-authored counts as substitutes.

The canonical analyzer is one DSLogic Plus with ten enabled channels:

| Raw bit | Signal |
| ---: | --- |
| 0 through 5 | Sensor 0 through sensor 5 INT1 |
| 6 | D8 aggregate snapshot |
| 7 | D9 cache commit |
| 8 | SDA |
| 9 | SCL |

The decisive run requests 7,220,000,000 words at 2 MHz, which is
14,440,000,000 raw bytes, and the scorer admits no less than 3,601 seconds.
The 20 MHz 10-second electrical preflight and 2 MHz 10-minute transport soak
are mandatory preparation but non-decisive. Build, finalization, hashing, and
scoring run in a Crabbox. Only the USB analyzer capture runs on the compatible
physical host.

Follow the capture guide's [Canonical artifacts](gate0a-capture-stack.md#canonical-artifacts),
[Box validation record](gate0a-capture-stack.md#box-validation-record),
[Physical preflight sequence](gate0a-capture-stack.md#physical-preflight-sequence),
[Physical-host capture](gate0a-capture-stack.md#physical-host-capture),
[Crabbox finalization](gate0a-capture-stack.md#crabbox-finalization), and
[GPL and firmware provenance](gate0a-capture-stack.md#gpl-and-firmware-provenance)
sections. The frozen firmware bytes are reproducible by identity, but their
mapping to available source is not. That remains a legal and supply-chain risk.

### Deterministic Gate 0A stop

Gate 0A uses one tested hard cut. The wrapper sends one private local stop byte
to the recorder when the raw capture first reaches 14,416,000,000 through
14,417,000,000 bytes. The recorder appends and transmits one exact `IKQ2`
request. Firmware continues ordinary acquisition until one scheduled aggregate
has six fresh committed samples and every admission and work state is idle.
That aggregate produces the one clean final D8 snapshot with the ordinary
2-through-less-than-20 us width and SensorFrame `N`.

After final D8 falls, firmware emits the exact 52-transaction, 1,422-SCL-rise
sensor shutdown suffix and then leaves the sampled port at `0x0300` for at
least 400 idle samples. It changes D8 and D9 in one atomic port write for the
exact `0x0300 -> 0x03c0 -> 0x0300` terminal marker. Marker width is 200 through
400 samples inclusive, and no one-pin intermediate `0x0340` or `0x0380` sample
is allowed. It commits one matching `IKR2` receipt only after marker fall and
emits no later application byte. The analyzer must retain at least 10,000,000
idle `0x0300` samples, or five seconds, after that marker. The host guards the
TTY through analyzer EOF and a further 100 ms of silence before writing
`END3`.

The exact byte layouts, lifecycle, transaction order, marker words, deadlines,
and failure behavior are normative in the
[Gate 0A deterministic quiesce contract](gate0a-quiesce-contract.md). Signals,
reset, power-off, manual trimming, a second session channel, or stdout text are
not success evidence.

## Checksum and stream envelope

CRC is the reflected IEEE CRC-32 with polynomial `0xedb88320`, initial state
`0xffffffff`, final XOR `0xffffffff`, and check value `0xcbf43926` for ASCII
`123456789`. CRC detects corruption; it is not authentication.

USB CDC is a byte stream. Each 160-byte frame is COBS encoded and followed by a
zero delimiter:

```text
COBS(frame) 00 COBS(frame) 00 ...
```

The maximum encoded candidate is 161 bytes; with its delimiter the maximum
transport record is 162 bytes. The receiver uses a fixed bound, retains
malformed and oversized candidates as evidence, and recovers at the next zero.
On disconnect it retains a nonempty unterminated candidate before it faults.

## Direction-specific Gate 0A control records

Only the Gate 0A recorder may send a controller control record. Calibration,
neutral collection, ordinary recording, replay, and live decoding reject any
control record.

### `IKQ2` quiesce request

The decoded request is exactly 84 bytes. Its COBS candidate is 85 bytes plus
the zero delimiter. All integers are little endian.

| Offset | Length | Field |
| ---: | ---: | --- |
| 0 | 4 | ASCII `IKQ2` |
| 4 | 1 | version `2` |
| 5 | 1 | hand: `0` left, `1` right |
| 6 | 1 | opcode `1`, quiesce |
| 7 | 1 | zero reserved |
| 8 | 32 | controller ID SHA-256 |
| 40 | 16 | boot ID |
| 56 | 16 | nonzero IKS3 session ID, also the request nonce |
| 72 | 8 | session-relative host request-start time in nanoseconds |
| 80 | 4 | CRC-32 of bytes 0 through 79 |

The recorder captures request-start time immediately before its first device
write. Firmware treats that value as opaque and echoes it. `tcdrain` proves
only that the host TTY drained, not that firmware accepted the request.

### `IKR2` quiesce receipt

The decoded receipt is exactly 88 bytes. Its COBS candidate is 89 bytes plus
the zero delimiter.

| Offset | Length | Field |
| ---: | ---: | --- |
| 0 | 4 | ASCII `IKR2` |
| 4 | 1 | version `2` |
| 5 | 1 | hand |
| 6 | 1 | kind `1`, quiesced |
| 7 | 1 | zero reserved |
| 8 | 32 | controller ID SHA-256 |
| 40 | 16 | boot ID |
| 56 | 16 | echoed IKS3 session ID |
| 72 | 8 | echoed request-start time |
| 80 | 4 | final normal SensorFrame sequence `N` |
| 84 | 4 | CRC-32 of bytes 0 through 83 |

Firmware accepts one matching request only while running. Any malformed,
oversized, unknown, mismatched, zero-session, or second inbound candidate
permanently suppresses the success marker and receipt. There is no retry.

## Source-aware IKS3 session

The host merges candidates by per-source receipt time, but each record retains
the source device independently of the payload hand. Receipt regression is
checked per source because valid left and right arrivals can interleave.

The fixed 64-byte header is:

| Offset | Length | Field |
| ---: | ---: | --- |
| 0 | 4 | `IKS3` |
| 4 | 1 | version `3` |
| 5 | 3 | zero reserved bytes |
| 8 | 4 | nominal per-hand period in microseconds |
| 12 | 32 | SHA-256 of canonical locked manifest JSON |
| 44 | 16 | random nonzero session ID |
| 60 | 4 | CRC-32 of bytes 0 through 59 |

Each record has a 28-byte prefix beginning with `REC3`, then its payload, then
CRC-32 of prefix bytes 4 through 27 plus the payload:

| Prefix offset | Length | Field |
| ---: | ---: | --- |
| 0 | 4 | ASCII `REC3` |
| 4 | 1 | record kind |
| 5 | 1 | physical source: `0` left, `1` right |
| 6 | 2 | zero reserved bytes |
| 8 | 4 | retained payload length |
| 12 | 8 | original length before bounded truncation |
| 20 | 8 | session-relative host observation time in nanoseconds |

Record kinds are:

| Kind | Meaning |
| ---: | --- |
| 0 | decoded SensorFrame |
| 1 | malformed candidate |
| 2 | oversized candidate |
| 3 | unterminated candidate |
| 4 | exact decoded outbound `IKQ2` request |
| 5 | exact decoded inbound `IKR2` receipt |

Kinds 0, 1, 3, 4, and 5 require retained and original lengths to match. Kind 2
requires original length to exceed the retained length. Request and receipt
records use the controller hand as their physical source and have separate
summary counts. They are neither SensorFrames nor invalid candidates.

The final 16 bytes are `END3`, the `u64` complete-record count, and CRC-32 of
those first 12 footer bytes. Missing footer, partial record, wrong count,
trailing bytes, invalid source, source/payload hand mismatch, or checksum error
cannot produce a `FinalizedSession`.

A successful Gate 0A session contains exactly one request and one receipt. The
clean SensorFrame whose sequence equals the receipt's final sequence is
immediately before the receipt. The receipt is the last record before `END3`.
The recorder writes the footer only after the analyzer-success guard and final
100 ms TTY silence. Other session consumers require zero control records.

Every pre-finish protocol, receipt, signal, disconnect, timeout, or guard
failure leaves no `END3`. Once the footer bytes are written, a later file or
parent sync failure or a strict-audit I/O failure can leave those bytes in a
hidden partial. That process exits nonzero and the wrapper publishes nothing.
The presence of `END3` bytes alone is not success evidence.

Malformed transport records remain valid evidence and increment the invalid
count. They cannot enter frame decoding, clock fitting, calibration, or live
output.

## Rate budget

At the selected 200 aggregate frames per second:

```text
160 raw bytes/frame * 200 frames/s = 32,000 raw bytes/s/hand
162 bounded transport bytes/frame * 200 frames/s = 32,400 bytes/s/hand
two hands = 64,800 bounded transport bytes/s
```

This fits native USB full-speed capacity. It does not prove the six-sensor I2C
schedule, OS buffering, or sustained loss-free delivery; Gate 0A measures
those facts.

## Fail-neutral rules

Invalid frame identity, loss, reorder, time regression, acquisition flags,
whole-hand silence, disconnect, decoder reset, or output failure disarms the
live runtime and releases all keys. It also terminally revokes the current
user-attested live instance; later good frames cannot rearm it. Watchdogs are
independent per hand, so one active controller cannot hide silence from the
other. Each restart reloads and checks the saved held-out evidence, then
requires a fresh per-process no-movement confirmation. If either wearable
moved, collect fresh held-out evidence before confirming because this protocol
cannot detect removal, movement, or loosening.

Expected latest-sample decimation is not an acquisition flag and does not
disarm the runtime. For each controller, both the raw aggregate timestamp and
every valid sensor's raw sample timestamp (aggregate device time plus its
sample-time offset) must strictly advance across frames, capture stages, and
computation-monitor handoffs, even at the permitted skew boundary. Repeated
aggregate or sensor timestamps, missing fresh samples, or any unclassified
read remain faults.

Offline calibration matching, keyboard prechecks, fitting, held-out validation,
contact-rule derivation, and independently derived neutral evidence admit the
complete raw stream per hand after identity canonicalization, before selecting
truth or neutral intervals or mapping timestamps. Admission checks acquisition
quality, wrapping sequence continuity, and strictly increasing raw aggregate
and sensor times, including frames outside every selected interval. A canonical
clock fit alone does not establish raw sample integrity. Source errors identify
the offending record and hand; invalid timing cannot reach neutral threshold
construction or decoding.

SensorFrame timestamps and offsets stay in raw controller time. Clock mapping
provides a separate output timestamp; it must not replace the aggregate time
while leaving sensor offsets in the raw domain.

Recording and output admission are separate. The recorder keeps the offending
candidate; the decoder must not use it.

## Executable checks

```text
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:firmware-frame-conformance-test
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:protocol-contract-test //:transport-contract-test
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:device-transport-contract-test //:session-contract-test
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //:firmware-acquisition-conformance-test
```

Rust and portable C fixtures pin SensorFrame, `IKQ2`, and `IKR2` bytes, COBS
envelopes, and CRC. Session tests pin IKS3 header, record kinds, footer, source
identity, context rejection, corruption, truncation, hard IKS2 rejection, and
Gate 0A terminal ordering. These are hardware-independent checks. Physical
rate, skew, quiesce timing, marker simultaneity, and signal quality remain
hardware-dependent.

## Change rule

There is no installed compatibility base. A byte-layout change replaces this
protocol, updates Rust and C conformance together, and keeps one canonical
parser. IKS3 is the only session container. Do not add an IKS2 reader, legacy
fallback, or side channel.
