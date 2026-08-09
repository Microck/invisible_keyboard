# Gate 0A deterministic quiesce contract

Status: normative implementation contract. The decisive physical capture
remains blocked until the code and Bazel contracts in this document pass.

This document defines one Gate 0A stop path. It is a hard cut. There is no
signal-completion fallback, IKS2 reader, alternate receipt file, manual trim,
or second controller path.

## Evidence sequence

One successful capture ends in this exact order:

```text
host appends and sends one stop request
zero or more ordinary SensorFrames
one clean scheduled final D8 snapshot and SensorFrame N
one exact 52-transaction sensor shutdown suffix
one simultaneous D8+D9 stop marker
one matching controller receipt for N
host and analyzer observe silence through analyzer EOF
host atomically commits the completed capture and closes signal admission
IKS3 footer is written and synced
```

The final D8 rising edge is both a normal aggregate boundary and the logical
DRDY admission cutoff. Firmware does not emit a second terminal frame or a
separate cut marker.

## Controller records

SensorFrame v2 stays exactly 160 decoded bytes. The same zero-delimited COBS
stream gains two direction-specific records. Every integer is little-endian.
Every checksum is the existing IEEE CRC-32.

### Quiesce request

The decoded `IKQ2` request is exactly 84 bytes. COBS produces 85 bytes plus
the zero delimiter.

| Offset | Bytes | Meaning |
|---:|---:|---|
| 0 | 4 | ASCII `IKQ2` |
| 4 | 1 | version `2` |
| 5 | 1 | hand: `0` left, `1` right |
| 6 | 1 | opcode `1`, quiesce |
| 7 | 1 | zero reserved |
| 8 | 32 | controller ID SHA-256 |
| 40 | 16 | boot ID |
| 56 | 16 | nonzero IKS3 session ID and request nonce |
| 72 | 8 | session-relative host request-start time in ns |
| 80 | 4 | CRC-32 of bytes 0 through 79 |

The recorder captures request-start time immediately before its first write.
Firmware treats that value as opaque and echoes it. `tcdrain` proves only host
TTY drain, not controller acceptance. A matching receipt closes that loop.

### Quiesce receipt

The decoded `IKR2` receipt is exactly 88 bytes. COBS produces 89 bytes plus
the zero delimiter.

| Offset | Bytes | Meaning |
|---:|---:|---|
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

The firmware accepts one matching request only while running. An empty,
malformed, oversized, unknown, wrong-hand, wrong-controller, wrong-boot,
zero-session, or second inbound candidate permanently suppresses success.
There is no retry.

## IKS3 hard cut

IKS3 keeps the fixed physical layout of IKS2 while changing its identity and
record taxonomy. No IKS2 reader or writer remains.

The 64-byte header is:

| Offset | Bytes | Meaning |
|---:|---:|---|
| 0 | 4 | ASCII `IKS3` |
| 4 | 1 | version `3` |
| 5 | 3 | zero reserved |
| 8 | 4 | nominal period in us |
| 12 | 32 | acquisition manifest SHA-256 |
| 44 | 16 | session ID |
| 60 | 4 | CRC-32 of bytes 0 through 59 |

Each record starts with the existing 28-byte prefix, with ASCII `REC3` at
bytes 0 through 3. The remaining prefix fields are unchanged:

| Offset | Bytes | Meaning |
|---:|---:|---|
| 4 | 1 | record kind |
| 5 | 1 | physical controller stream |
| 6 | 2 | zero reserved |
| 8 | 4 | captured payload length |
| 12 | 8 | original payload length |
| 20 | 8 | session-relative host observation time in ns |

The record CRC covers prefix bytes 4 through 27 followed by the payload.
Record kinds are:

| Kind | Meaning |
|---:|---|
| 0 | decoded SensorFrame |
| 1 | malformed candidate |
| 2 | oversized candidate |
| 3 | unterminated candidate |
| 4 | exact decoded outbound `IKQ2` request |
| 5 | exact decoded inbound `IKR2` receipt |

Kinds 0, 1, 3, 4, and 5 require captured and original lengths to match. Kind
2 requires original length to exceed captured length. The physical source
field identifies the controller hand for both control records.

The 16-byte footer uses ASCII `END3`, the existing 64-bit record count, and
the existing footer CRC. `SessionSummary` counts requests and receipts
separately. They are neither decoded SensorFrames nor invalid candidates.
Calibration, neutral, replay, and live consumers reject control records unless
their command contract explicitly owns them.

A successful Gate 0A session contains exactly one request and one receipt. The
receipt is the last record, immediately follows clean SensorFrame `N`, and is
followed only by `END3` after the analyzer-linked guard succeeds.

## Firmware lifecycle

The existing portable glue policy owns the only lifecycle:

```text
Running
  -> StopRequested
  -> BoundaryClosed
  -> ShuttingDown
  -> Marking
  -> ReceiptPending
  -> Quiesced
```

A permanent failure bit can latch from any state. It suppresses the success
marker and receipt. Firmware never retries, resets, or resumes acquisition.

All aggregate, UART, timer, GPIO-admission, cache, and transport decisions use
one documented lock order:

```text
glue policy -> sample cache -> sensor admission state
```

The implementation must represent at least these sensor states so the current
admission-to-work-submission gap cannot disappear from the predicate:

```text
Idle
AdmittedUnsubmitted
Submitted
Running
Closed
```

The DRDY callback enters `AdmittedUnsubmitted` before work submission and
records either `Submitted` or a permanent submission fault before leaving its
critical section.

## Safe final boundary

After a valid request, normal scheduled 200 Hz aggregates continue until one
aggregate satisfies all of these conditions atomically:

- all six cache slots contain fresh committed samples;
- no cache or acquisition fault is latched;
- every sensor admission state is `Idle`;
- no work is unsubmitted, queued, running, committing, or holding D9 high;
- the final-frame transport slot and one receipt slot are reserved;
- the scheduled target can still meet the ordinary frame deadline.

Firmware has 400 ms from request acceptance to find this boundary. It does
not synthesize an empty or off-cadence terminal aggregate.

With GPIO callbacks excluded and all three locks held, the aggregate performs
this exact boundary operation:

1. Raise D8.
2. Change the lifecycle to `BoundaryClosed`.
3. Change all six sensor admission states to `Closed`.
4. Copy and clear the complete cache.
5. Hold D8 for the ordinary 2 through less-than-20 us interval.
6. Lower D8.
7. Release the locks.
8. Disable all six GPIO interrupts.
9. Stop the aggregate timer and reject every stale aggregate wake.
10. Plug and drain the sensor work queue, which must already be empty.
11. Build clean SensorFrame sequence `N` for the scheduled target.
12. Require flags zero, valid sensor mask `0x3f`, and an immutable final-slot
    insertion.

D8 rises before admission closes inside the same IRQ-excluding critical
section. A physical INT1 edge at or after that D8 rise is shutdown-unadmitted.
A physical edge before D8 whose callback did not linearize in time remains an
unmatched pre-cut edge and rejects the capture. The scorer never forgives that
edge as a boundary special case.

## Ordered terminal transport

Replace the one application slot with one fixed three-entry ordered ring:

1. at most one normal streaming record;
2. one slot reserved for final SensorFrame `N`;
3. one slot reserved for the receipt.

Normal streaming cannot consume the terminal slots. Before final D8, at most
one older frame can remain. The final frame follows it. Receipt capacity is
therefore reserved before irreversible shutdown, although receipt bytes are
not committed until after marker fall. The ring and the existing driver FIFO
must preserve this order.

## Exact sensor shutdown suffix

After final D8 falls and final SensorFrame `N` enters its ring slot, firmware
emits exactly 52 I2C transactions:

1. TCA9548A address `0x70`: write mask `0x00`, then read back `0x00`.
2. For sensor 0 through 5, in order:
   1. write one-hot TCA mask, then read the same mask;
   2. write IMU `INT1_CTRL` register `0x0d = 0x00`, then read zero;
   3. write `CTRL1_XL` register `0x10 = 0x00`, then read zero;
   4. write `CTRL2_G` register `0x11 = 0x00`, then read zero.
3. Write final TCA mask `0x00`, then read back `0x00`.

Normal acquisition keeps its one-hot channel-selection API. Shutdown uses a
separate deselect-all operation. It does not expose arbitrary mux masks to
normal acquisition.

The raw shapes are fixed:

- one-byte TCA write: 18 SCL rises;
- one-byte TCA read: 18 rises, final byte NACKed;
- two-byte IMU write: 27 rises;
- register write plus repeated-start one-byte read: 36 rises, final byte
  NACKed;
- complete suffix: 1,422 SCL rises.

A missing, extra, reordered, NACKed, corrupt, truncated, timed-out, or wrong
readback transaction suppresses the marker and receipt.

## Terminal marker

After the final TCA read:

1. Require SDA and SCL high and all six INT1 inputs low.
2. Wait 200 us and check the endpoints again.
3. Require the analyzer to observe at least 400 idle samples of `0x0300`.
4. Set D8 and D9 together with one GPIO1 port operation.
5. Request a nominal 150 us hold.
6. Clear both with one GPIO1 port operation.
7. Never change D0 through D9 again.

The normative words and bytes are:

```text
idle      0x0300  00 03
marker    0x03c0  c0 03
terminal  0x0300  00 03
```

Both bits must change in one analyzer sample. No intermediate `0x0340` or
`0x0380` sample is allowed. Marker width is 200 through 400 samples inclusive,
or 100 through 200 us at 2 MHz. INT1 stays low, SDA and SCL stay high, and
reserved bits stay zero throughout the marker.

Firmware commits the receipt slot only after marker fall. It emits no later
application byte.

## Host recorder and analyzer supervision

`capture-gate0a.py` owns sigrok and the duplex Gate 0A recorder. A public stop
socket or helper command is not part of the contract.

The recorder opens one real TTY with `O_RDWR`, raw 921600 8N1 settings, and
exclusive access. A PTY is admitted only by hardware-independent contract
tests. Signals, disconnects, and EOF never mean successful quiescence.

The controller starts held in reset so the analyzer retains the boot prefix.
Before its first open, the recorder allows up to three seconds for the declared
application TTY to appear. Only an absent path is retried; other open or terminal
errors fail immediately. Signals abort this wait. Once the TTY opens, there is
no reopen or reconnect path. The wrapper must observe readiness before raw size
exceeds 12,000,000 bytes, in addition to its wall-clock startup deadline.

The wrapper and recorder use two private pipes:

- recorder to wrapper: one readiness byte after identity bootstrap and IKS3
  header creation;
- wrapper to recorder: one `0x01` stop byte, kept open as the analyzer guard.

The wrapper polls raw size at intervals no greater than 10 ms. It sends the
stop byte on the first observation at or above 14,416,000,000 bytes. That
first eligible observation must be no greater than 14,417,000,000 bytes. The
wrapper closes the pipe only after sigrok exits successfully, the raw file is
exactly 14,440,000,000 bytes and synced, and the final analyzer log passes its
quota, failure-marker, driver-receipt, and staged-firmware-open checks.

The recorder performs this exact flow:

1. Bootstrap and lock controller identity.
2. Create IKS3 and publish readiness.
3. Accept exactly one local `0x01` byte after readiness.
4. Capture request-start time and append the exact outbound request record.
5. Send the COBS request and call `tcdrain` in a dedicated writer thread.
   Keep the supervisor servicing sensor input, the guard pipe, and signals.
6. Require successful write/drain completion and one matching receipt within
   750 ms of the same request start. Continued sensor traffic or a blocked
   writer does not extend this deadline. A timed-out writer is not joined;
   recorder process exit closes the connection without `END3`.
7. Timestamp the receipt after the `read()` that supplied its delimiter.
8. Reject any byte after the receipt, including a partial COBS candidate.
9. Keep observing the TTY until analyzer-success EOF on the private pipe.
10. Observe a final 100 ms of TTY silence.
11. Atomically change the host signal state from open to footer-committed.
12. Write `END3`, flush, sync the file and parent directory, strictly audit it,
    and only then publish success.

EOF before the receipt, an extra local byte, a second request, a duplicate or
mismatched receipt, any post-receipt byte, a signal accepted before step 11,
disconnect, timeout, or analyzer failure leaves the session without `END3`.
The SIGINT/SIGTERM handler and step 11 modify the same atomic word, so exactly
one wins. If the signal store wins, the recorder does not call
`SessionWriter::finish`. If the footer-commit transition wins, a later signal
is outside the completed-capture boundary and cannot revoke `END3`. A later
footer write, sync, or strict-audit failure still exits nonzero and leaves the
hidden partial for wrapper failure handling; it does not roll the file back.

The hardware wrapper publishes raw, IKS3, locked manifest, logs, and the small
identity artifacts only after both children succeed. It does not hash the
14.44 GB raw file and does not claim marker indices. The Crabbox finalizer
hashes the raw and IKS3 files, runs the one raw reducer, and binds their hashes
and derived terminal fields into the decisive sidecar.

## Time and raw deadlines

| Event | Limit |
|---|---:|
| Request acceptance to clean final boundary | 400 ms |
| Request acceptance to receipt offer | 500 ms |
| Host request start to receipt observation | 750 ms |
| Stop request raw position | first observation from 14,416,000,000 through 14,417,000,000 bytes |
| Marker fall | no later than raw sample 7,210,000,000 |
| Raw EOF | exactly sample 7,220,000,000 |
| Idle tail after marker | at least 10,000,000 samples, or five seconds |
| Host silence guard | through analyzer EOF plus 100 ms |

A synchronous I2C call that returns after its success deadline suppresses the
receipt. A driver call that never returns is rejected by the independent host
timeout. Firmware does not watchdog-reset because reset would emit another
boot anchor and restart D0 through D9 activity.

## Failure state

Before boundary closure, a protocol or safe-boundary timeout latches failure
and emits no marker or receipt. Acquisition can continue until the supervisor
aborts so firmware does not discard admitted work to fabricate a clean stop.

After boundary closure:

- admission stays closed;
- timer and GPIO interrupts stay disabled;
- marker pins are forced low where possible;
- no success receipt follows any frame, bus, marker, or transport fault;
- partial shutdown remains visible in raw evidence;
- firmware never resets or resumes acquisition.

Host timeout and the raw scorer reject every incomplete path.

## Gate 0A scorer

The scorer first audits IKS3:

- exactly one request and one receipt;
- exact source, hand, controller, boot, session, and echoed time binding;
- host response at most 750 ms;
- clean SensorFrame `N` immediately before the receipt;
- receipt final sequence equals `N`;
- receipt is the last record.

Raw reduction then uses the D8 linked to `N` as the cutoff:

1. Every D8 through `N` is an ordinary snapshot.
2. D8 `N` links to the exact clean final frame.
3. Every INT1 rise before D8 `N` satisfies normal acquisition accounting.
4. Every INT1 rise at or after D8 `N` is shutdown-unadmitted.
5. No acquisition I2C transaction or normal D9 is legal at or after D8 `N`.
6. After final D8 fall, decode the exact 52-transaction suffix.
7. Recognize the simultaneous marker before ordinary D8/D9 handlers.
8. Require the marker exactly once with the exact width and words.
9. Require every later raw word to equal `0x0300`.
10. Require at least 10,000,000 terminal idle samples.

Normal pre-cut accounting stays:

```text
INT1 rises = committed reads
committed reads = selected reads + deliberately decimated reads
selected reads = linked reads + host-dropped reads
```

Shutdown-unadmitted rises are reported separately. The Gate 0A report exposes
`deterministic_quiesce` as an explicit criterion and records final sequence,
suffix transaction count, marker indices and width, shutdown-unadmitted counts,
host response time, and terminal idle length.

## Hardware-independent acceptance matrix

All builds and tests run through Bazel in a Crabbox. Required tests use
production code with deterministic clocks, PTYs, and in-memory bus fixtures.
They do not use mocked modules.

- C/Rust request and receipt goldens, COBS boundaries, reserved bytes, CRC,
  identity, direction, partial candidates, and oversize rejection;
- IKS3 header, records, footer, control counts, context rejection, truncation,
  and IKS2 hard rejection;
- request on both sides of aggregate linearization;
- empty, partial, full, and faulted cache states;
- every admission and admitted-before-submit state;
- every pending I2C, D9, cache commit, timer, and transport state;
- terminal ring backpressure and every FIFO consumption split;
- every one of the 52 shutdown transactions omitted, duplicated, reordered,
  NACKed, corrupted, or truncated;
- marker widths 199, 200, 400, and 401 samples;
- staggered dual-pin transitions and every post-marker D0 through D9 change;
- idle tails 9,999,999 and 10,000,000 samples;
- request or receipt loss, duplication, mismatch, corruption, and timeout;
- post-receipt bytes in the same read, at 100 ms, and near analyzer EOF;
- PTY plus private pipes through finalized IKS3;
- raw-size observations before, inside, and beyond the request window;
- recorder or analyzer failure preventing publication;
- one native controller path using the production glue, cache, shutdown,
  marker, receipt, and terminal-ring implementations.

## Claims that remain physical-only

Hardware-independent tests cannot prove:

- physical INT1 and D8 threshold order at the cutoff;
- one-sample threshold simultaneity for the D8/D9 port operation;
- captured marker width;
- real I2C ACK, clock stretching, repeated-start shape, or driver return time;
- actual IMU power-down and future INT1 deassertion;
- real CDC ordering, latency, and backpressure behavior;
- partial raw-file visibility before sigrok flushes data;
- DSLogic clock accuracy, probe attachment, or voltage margin;
- silence beyond the finite captured tail.

The raw trace decides observable waveform claims. Native tests decide software
admission and queue state. The request, receipt, identity, session ID, and
final sequence bind the host evidence to the analyzer path. Proving the USB
request itself caused the marker would require another analyzer-visible wire,
which Rev A does not add.
