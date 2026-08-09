# Selected-board firmware

This directory owns the Zephyr 4.4.0 application for one XIAO nRF52840 Plus
per hand. Bazel is the canonical entry point. It produces left and right ELF,
Intel HEX, and bootloader-ready UF2 images plus one provenance record.

## Controller provisioning

The host manifest defines a `hands[].controller_id` string and checks that the
wire frame contains SHA-256 of its exact UTF-8 bytes. The two canonical image
inputs are explicit:

- `left-hand.conf`: `invisible-keyboard-left-001`
- `right-hand.conf`: `invisible-keyboard-right-001`

`main.c` hashes the selected string without a prefix or separator. The native
conformance target checks the left value against the same SHA-256 bytes the
host computes. Do not flash either image onto another logical controller
without assigning a new string and making the acquisition template match it.

The cross-build writes
`invisible-keyboard-xiao-nrf52840-plus.provenance.txt`. Its `firmware_build`
line hashes both deployable UF2 digests into one build-set identity because the
host manifest has one build field for the two hand images. Copy that full value
into the locked acquisition manifest. The same record contains both plain
controller IDs, their wire hashes, dependency versions, and each artifact
digest.

The Bazel action passes one mandatory `--cache-dir` below its configuration
output directory. The build script never reads `HOME`, `XDG_CACHE_HOME`, or a
process-wide cache override. Concurrent workspaces and configurations cannot
share mutable Zephyr or SDK state by accident.

The cross-build keeps full ELF symbols and DWARF but maps the repository,
cache, and temporary build roots to stable virtual paths in debug data. It
rejects either published ELF if a real executor path remains embedded. This
keeps all seven declared outputs independent of the Bazel output base without
stripping the debug data or changing the loadable firmware used during board
bring-up.

The locked SDK install matches the 1.0.1 release bundle workflow: the split
ARM GNU archive is extracted under `gnu/`, and the host tools installer from
the pinned minimal archive is run non-interactively. CMake receives the exact
SDK root and `zephyr/gnu` variant, so the build does not use the user package
registry or an ambient toolchain.

The USB serial descriptor can expose the MCU hardware identity during physical
bring-up, but protocol v2 has no separate hardware-ID field. Binding each
logical controller ID to the actual board serial number is therefore a flash
and inspection record, not a claim made by this source-only build.

## Physical flash handoff

Build only through the Bazel target in a Crabbox. That target publishes these
named application images:

| Hand | Logical controller ID | UF2 artifact |
| --- | --- | --- |
| Left | `invisible-keyboard-left-001` | `bazel-bin/firmware/zephyr/invisible-keyboard-left-xiao-nrf52840-plus.uf2` |
| Right | `invisible-keyboard-right-001` | `bazel-bin/firmware/zephyr/invisible-keyboard-right-xiao-nrf52840-plus.uf2` |

The adjacent
`bazel-bin/firmware/zephyr/invisible-keyboard-xiao-nrf52840-plus.provenance.txt`
is part of the handoff. Do not rename a UF2 to `firmware.uf2`, use an image from
a CMake build directory, or choose a file by modification time. Before copying
either image to a physical host, extract its expected digest from the
provenance record and compare it with `sha256sum`. For the left image, for
example:

```sh
IKB_FW_DIR=bazel-bin/firmware/zephyr
IKB_UF2="$IKB_FW_DIR/invisible-keyboard-left-xiao-nrf52840-plus.uf2"
IKB_PROVENANCE="$IKB_FW_DIR/invisible-keyboard-xiao-nrf52840-plus.provenance.txt"
IKB_EXPECTED_SHA256=$(awk -F= '$1 == "left_uf2_sha256" { print $2 }' "$IKB_PROVENANCE")
test -n "$IKB_EXPECTED_SHA256"
test "$(sha256sum "$IKB_UF2" | awk '{ print $1 }')" = "$IKB_EXPECTED_SHA256"
```

Use `right_uf2_sha256` and the named right image for the other hand. Copy the
UF2 and provenance record together, then repeat the digest check on the
physical host. A missing key, mismatch, or duplicate candidate image stops the
flash.

Flash and bind one disconnected controller at a time:

1. Disconnect every other XIAO. Record the received board revision, visible
   serial or lot mark, and a SHA-256 hash of a photo that shows that mark.
2. Connect the board over USB with `PWR_LINK` open. Record the USB topology path
   and all host-reported vendor, product, model, and serial properties. Do not
   treat `/dev/ttyACM0` or another ordinal device name as identity.
3. Double-tap reset. The upstream Zephyr board instructions say the shipped
   bootloader presents a mass-storage device named `XIAO BLE` and accepts a UF2
   copied to its root [xiao-zephyr-board]. Record the actual volume label, block
   device topology path, USB serial, `INFO_UF2.TXT` bytes and SHA-256, and its
   reported model, board ID, and bootloader version. Stop if the volume,
   information file, or XIAO nRF52840 identity is absent or ambiguous.
4. Copy only the hand-specific, digest-verified Bazel UF2 to that verified
   mount. Wait for the copy and filesystem sync to finish. The bootloader
   should unmount and reset into the application. Never remove USB while the
   copy is in progress.
5. Record the application CDC device's USB topology path, vendor/product IDs,
   serial property, and stable `/dev/serial/by-id` link. Require the physical
   topology to match the sole board from steps 1-3. If the bootloader and
   application expose different serial strings, record both and bind them by
   this one-board-at-a-time topology transition. If either side lacks a stable
   serial, the serial-binding gate is inconclusive and the board is not
   admitted to a two-controller run.
6. Run the Gate C identity capture and require the first accepted frame to
   contain the SHA-256 of the exact logical controller ID in the table. Store
   the frame hash, UF2 hash, provenance hash, bootloader record, application
   serial record, physical photo hash, and logical controller ID in one
   immutable flash record.
7. Disconnect the admitted board before repeating the process for the other
   hand. Connect both only after their separate records prove different
   physical serials, different logical controller IDs, and the intended left
   and right UF2 digests.

UF2 copy proves only that the bootloader accepted a file. It does not prove the
application bytes now in flash, pin continuity, or acquisition behavior. Gate
C and Gate 0 retain those physical proof duties.

## Electrical contract

The overlay uses only upstream-source-verified XIAO header pins:

- D4/P0.04 SDA and D5/P0.05 SCL at 400 kHz
- D0, D1, D2, D3, D6, and D7 for sensor INT1 inputs
- D8/P1.13 for the analyzer-only aggregate-snapshot marker
- D9/P1.14 for the analyzer-only cache-commit marker
- TCA9548A A0-A2 low for address 0x70
- every LSM6DS3TR-C SA0 low for address 0x6a
- TCA9548A RST breakout pad left open; the breakout's onboard 10K pull-up holds
  reset inactive

The firmware has no mux-reset GPIO. The open external RST pad and onboard
pull-up are one physical contract, not a software-controlled reset. Whole-
controller power cycle or fatal stop is the Rev-A recovery path. The inspected
[Adafruit breakout schematic][tca-schematic] is the authority for the onboard
pull-up.

The external TCA and sensor rail must not be live while the controller is
unpowered. During split-rail Gate B work, keep `PWR_LINK` open, power the XIAO
through the qualified current-limited USB path first, enable only a voltage-
matched current-limited fanout rail, and turn that fanout rail off and prove it
discharged before removing USB. Admission requires the fixed 80 mA input limit,
64 mA operating envelope, source-response measurements, and linked thermal trial
in the [Rev-A assembly baseline](../../docs/implementation/rev-a-assembly-baseline.md#gate-b-current-limited-bench-power).
Keep that exact qualified power path for subsequent supervised trials. This is
an experimental measured envelope, not a manufacturer external-current
guarantee or safety certification.

Both markers start low. Marker operations use the documented lock prefix of
glue policy, sample cache, then sensor admission state. Immediately after GPIO
configuration, before sensor I/O or interrupt setup, firmware emits this boot
anchor:

1. D8 high with a 20 us minimum hold.
2. Both markers low with a 20 us minimum hold.
3. D9 high with a 20 us minimum hold.
4. Both markers low with a 20 us minimum hold.

Arm the analyzer before a full sensor/controller power cycle, or a controller
reset after verified sensor quiescence. Gate 0 consumes the boot anchor only
as the first trace prefix, while no INT1 or I2C activity exists.
It does not infer the anchor from pulse width alone. A missing, partial, or
out-of-order prefix invalidates the capture. Any GPIO error fails firmware boot
instead of claiming a valid prefix.

Normal D9 and D8 pulses each include a 2 us minimum-high hold after their
evidence operation; source does not claim a maximum pulse width. D9 rises
before a completed read enters its cache. The worker stores the sample, waits
at least 2 us, and lowers D9 while it still holds the sensor-state lock. It
then clears that sensor's pending state before releasing either lock. Thus a
successful D9 fall is the linearization point for both cache eligibility and
admission of the next same-sensor edge. D8 rises before the first of six cache
slots is copied, then waits at least 2 us after the final slot is consumed
before falling. The shared cache lock prevents D8 and D9 from overlapping.

The first normal D8 after the boot anchor produces frame sequence 0. Every
later normal D8 advances sequence exactly once, whether or not a host has the
CDC device open. A received frame with sequence N therefore maps to normal D8
ordinal N only when the capture includes the boot anchor and every intervening
D8 pulse. Host open, close, disconnect, and reconnect never restart or splice
sequence. These pins are test instrumentation, not sensor inputs. A rear-pad
reset output remains omitted. The overlay disables the upstream hardware UART
because its default D6/D7 pins are sensor interrupts.

## Startup admission

The startup trace validates each of the six mux selections and readbacks,
software reset, at most 100 reset polls with 100 us retry delays, `WHO_AM_I`,
and every configured register readback. Initialization leaves `INT1_CTRL=0`.
Only after all six profiles pass does main install every GPIO callback.

One startup work item then arms the bank on the sensor work queue. It selects
each channel, writes `INT1_CTRL=1`, and reads the value back. Sensor jobs queued
by those new interrupts cannot run until the arming item completes, so they
cannot change the mux during startup. Any failed transfer or readback fails
boot.

The analyzer opens each sensor's admission at the START of its known
interrupt-enable write. This includes a pulse that rises before that write's
STOP or readback. Every subsequent pulse receives strict pending-read and
cache-commit accounting. Unknown/reset-interval pulses remain separate startup
facts. A pulse after a sensor's validated quiet readback but before its enable
write invalidates the capture. There is no exemption for missed admitted edges
before a sensor's first read.

At successful arm-work completion, firmware fixes an absolute 100 ms deadline
for the first clean sample in every cache slot. Main scheduling cannot restart
that deadline; a fault or timeout fails boot. The analyzer measures the cap
from the final arming readback STOP. The existing 200 Hz timer starts only after
priming succeeds; its 5000 us period does not change.
The first normal D8 establishes the phase grid. A sample replaced before that
snapshot is explicit pre-grid decimation, not a timing violation or a
fabricated histogram entry. Pre-grid counts plus phase-histogram counts must
equal all decimated reads.

The trace reports the final arming readback as `startup_end_sample` and the
first six-slot clean cache as `first_clean_cache_sample`.
`pre_snapshot_decimated_reads` records each sensor's pre-grid replacements.

## Deterministic Gate 0A stop

The [Gate 0A quiesce contract](../../docs/implementation/gate0a-quiesce-contract.md)
is the sole authority for lifecycle, record bytes, deadlines, shutdown order,
marker semantics, and failure behavior. This Zephyr adapter realizes it with:

- one 85-byte encoded RX accumulator feeding the shared COBS and `IKQ2`
  decoder;
- one glue-policy spinlock, followed only by cache and sensor-state locks in
  that order;
- six static `k_work` items whose documented Zephyr submission results `1`
  and `2` both enter the explicit `Submitted` state;
- one plugged `k_work_q`, where a successful terminal drain must return `0`
  because the safe-boundary predicate already excludes queued or running work;
- three enum-indexed static transport slots serviced only by the CDC interrupt
  callback;
- the normal one-hot mux helper plus a zero-mask entry point used only by the
  shutdown suffix;
- direct GPIO0 input-register reads for fixed SDA P0.04 and SCL P0.05 without
  reconfiguring the pins away from TWIM; and
- one masked GPIO1 port operation for each simultaneous D8+D9 transition.

Both terminal paths block the main thread with `K_FOREVER`. The USB stack and
interrupt handling can continue, while firmware never schedules a periodic
terminal wake, resets, or restarts acquisition.

## Runtime bounds

Each IMU runs at nominal 208 Hz with `DRDY_PULSE_CFG_G=0x80`, producing an
approximately 75 us accelerometer data-ready pulse on INT1. The worker polls
gyro-ready at most 20 times with at most 190 us of inserted retry delay, then
reads the 12-byte gyro/accelerometer block. Every read performs a mux write,
10 us settle, mux readback, one to twenty `STATUS_REG` reads, then the 12-byte
burst from 0x22. The sensor FIFO stays in reset-default bypass mode, so the
firmware neither reads `FIFO_STATUS2` nor claims FIFO loss evidence.

Six fixed sensor work items feed one serial work queue and one latest-sample
cache slot per sensor after the startup item finishes. The independent
aggregate cadence is 200 Hz, or 5000 us. A
new completed 208 Hz read may replace an older read that no aggregate selected;
this is deliberate decimation and does not set acquisition overrun. The exact
data-ready edge timestamp remains attached to the selected reading. A pulse
while its sensor read/work is still pending, a bus/read/configuration failure,
missing fresh sample, missed aggregate deadline, invalid offset, application
slot backpressure, or synchronous FIFO admission failure still latches a real
acquisition fault.

The aggregate timer uses one-shot absolute tick deadlines derived from a
single startup uptime anchor and exact 5000 us target increments. Each target
is rounded up independently. At the selected 32768 Hz tick rate, adjacent
deadlines are 163 or 164 ticks apart; quantization remains less than one tick
instead of accumulating the error of a repeating 164-tick period. Frame times
remain on the exact 200 Hz timeline and retain the ordinary target-plus-5000-us
completion deadline.

The pinned Zephyr [timer implementation](https://github.com/zephyrproject-rtos/zephyr/blob/684c9e8f32e4373a21098559f748f06915f950c9/kernel/timer.c)
releases its timer lock before invoking the expiry callback. Its
[timeout implementation](https://github.com/zephyrproject-rtos/zephyr/blob/684c9e8f32e4373a21098559f748f06915f950c9/kernel/timeout.c)
replays due absolute deadlines during a late clock announcement and exposes
the scheduled event time as callback uptime. The callback therefore rearms
from the prior logical target, never from observed callback uptime. Every
expired period enters the existing pending-period accounting; delayed or
in-flight acquisition still latches overrun and cannot select a final boundary.
Rearming and stopping share the glue-policy lock, and closed admission or an
explicit terminal stop prevents another rearm.

`//firmware/zephyr:native-glue-conformance-test` includes a deterministic
one-hour, 32768 Hz cadence case, real acquisition flag/deadline checks, missed
and in-flight periods, and final-boundary recovery. It does not replace the
selected-board build or physical analyzer timing evidence.

USB uses Zephyr's next-generation native CDC ACM stack. The devicetree fixes
its TX FIFO at 324 bytes. The application owns one reusable 162-byte normal
slot plus fixed final-frame and receipt slots reserved by an accepted stop
request. Only the CDC interrupt callback calls `uart_fifo_fill`, checks its
returned byte count, and advances the ordered slots. Firmware does not poll
DTR. In the pinned Zephyr source,
[`SET_CONTROL_LINE_STATE` only records DTR][zephyr-dtr],
[`cdc_acm_fifo_fill` admits bytes based only on ring-buffer space][zephyr-fill],
and [class disable does not clear DTR][zephyr-disable]. DTR is therefore neither
required nor reliable as a per-frame delivery signal.

Every ordinary encoded frame is offered only to the reusable normal slot.
Before USB is configured, the driver can retain two frames in its fixed FIFO
and one in that application slot; later ordinary offers fail and latch
acquisition overrun until space returns. Active endpoint transfers use the
stack's fixed buffer pool, not an input-sized queue. Endpoint failure after
FIFO acceptance is visible through a host sequence gap because the pinned
driver exposes no completion result to this application. Acquisition and
sequence never depend on host or DTR state, so analyzer ordinal N remains tied
to frame sequence N across reconnect.

A physical disconnect can therefore recover at most three complete old frames
plus one fixed endpoint fragment. The live host treats 100 ms of source silence
as terminal. A new run clock-warms before decoder admission, and this bounded
device backlog is too short to satisfy its 100 ms clock-fit span; a minutes-later
fresh frame instead makes the incomplete clock epoch fail closed. This does not
cover bytes that USB already delivered into an unconsumed host-kernel buffer.
CDC provides no application-delivery acknowledgement, so safe close-and-reopen
semantics for that separate case require a host credit or acknowledgement
protocol, not DTR polling.

The Zephyr development VID/PID `2fe3:0004` is suitable only for local bring-up;
distribution requires an assigned product USB identity.

[zephyr-dtr]: https://github.com/zephyrproject-rtos/zephyr/blob/684c9e8f32e4373a21098559f748f06915f950c9/subsys/usb/device_next/class/usbd_cdc_acm.c#L536-L540
[zephyr-fill]: https://github.com/zephyrproject-rtos/zephyr/blob/684c9e8f32e4373a21098559f748f06915f950c9/subsys/usb/device_next/class/usbd_cdc_acm.c#L770-L791
[zephyr-disable]: https://github.com/zephyrproject-rtos/zephyr/blob/684c9e8f32e4373a21098559f748f06915f950c9/subsys/usb/device_next/class/usbd_cdc_acm.c#L374-L382
[tca-schematic]: https://github.com/adafruit/Adafruit-TCA9548A-I2C-Multiplexer-PCB/blob/master/Adafruit%20TCA9548A.sch
[xiao-zephyr-board]: https://docs.zephyrproject.org/4.4.0/boards/seeed/xiao_ble/doc/index.html

Rev A has no readable queue-depth or numeric drop-counter telemetry. Host
sequence gaps plus the recovered frame's acquisition-overrun flag or a retained
malformed transport candidate provide fault accounting. Gate 0A independently
decodes the boot anchor, quiet configuration, arming sequence, six pulsed INT1
lines, D9 commit marker, D8 snapshot marker, and SDA/SCL. It assigns per-sensor
read ordinals, classifies every admitted motion read as selected or decimated,
proves newest-committed-at-snapshot selection, and binds selected reads to
unique raw host payloads. It reports startup facts, edge/ODR/jitter/age/phase,
pre-grid decimation, and the 1-to-20 status-poll histogram. A read that finishes
after the next admitted edge for that sensor invalidates the capture.

## Verification boundary

Run from the repository root:

```sh
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch test //firmware/zephyr:native-conformance-test //firmware/zephyr:native-glue-conformance-test --test_output=errors
box exec --cwd invisible_keyboard BOX_ID bazelisk --batch build //firmware/zephyr:firmware --verbose_failures
```

The native targets exercise the production quiet initialization, arming,
cache readiness, shutdown, terminal marker, transport slots, and glue lifecycle
through in-memory transcript fixtures. The raw-trace contract also converts
`native-conformance-test --startup-transcript` output into an I2C waveform and
runs the production reducer against it. This binds startup acceptance to the
portable C implementation rather than a separately invented success sequence.
The lifecycle cases cover the admitted-before-submission state, request
placement on both sides of aggregate linearization, cache/sensor/deadline
gates, inclusive firmware deadlines, partial FIFO consumption, terminal
ordering and silence, exact 52-call shutdown order and per-position mutations,
normal marker ordering, frame sequence 0, controller hash, boot entropy
failure, and the exact SensorFrame COBS golden. The cross target compiles and
links both hands for `xiao_ble/nrf52840` with the explicit Plus overlay and
checksum-locked Zephyr and SDK inputs.

Neither target proves physical INT1 versus D8 threshold order at the cutoff,
one-sample D8/D9 threshold simultaneity, captured marker width, real I2C ACK or
clock-stretch behavior, repeated-start shape, I2C driver return time, actual
IMU power-down and future INT1 deassertion, CDC latency and backpressure,
soldered pin continuity, mux reset level, DSLogic clock accuracy, probe
attachment, voltage margin, flash behavior, or silence beyond the finite raw
tail. Those checks require the two physical controller assemblies and a logic
analyzer.
