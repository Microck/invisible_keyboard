# Gate 0A DSLogic capture stack

Status: capture, recorder supervision, and finalization are implemented. The
focused hardware-independent contracts have passed in a Crabbox. The complete
frozen-tree validation report remains the pass authority. Physical preflights
and the decisive capture have not run and remain hardware-dependent gates.

This guide defines the only admitted Gate 0A analyzer path for Rev A. It uses a
DreamSourceLab DSLogic Plus, a pinned and fail-closed sigrok stack, an exact
3,610-second raw capture, and a scorer that reads every 16-bit sample word. No
human-authored timing total, decoded I2C transaction, or pass claim enters the
decisive sidecar.

## What this path proves

The software path can prove all of the following before hardware exists:

- the capture executable contains the patched DSLogic driver rather than
  loading an ambient `libsigrok.so`;
- the executable, driver patch, capture log, and device firmware bundle have
  named SHA-256 identities;
- the process requested 7,220,000,000 timepoints at 2 MHz on channels 0 through
  9;
- every USB block that contributed samples completed with a valid non-empty
  DSLogic block length;
- the raw output is exactly 14,440,000,000 bytes, or 7,220,000,000 little-endian
  `u16` words;
- the raw and log files were closed and synced before the success receipt; and
- the exact IKS3 recorder binary, session, locked manifest, rig evidence, axis
  characterization, and acquisition template are published with the raw
  capture; and
- the resource-heavy raw and IKS3 hashes and the same-binary score are computed
  inside a Crabbox.

It cannot prove that the analyzer clock is accurate, probes remained attached,
2 MHz observed every analog transition, the voltage threshold had adequate
margin, or a physical host can sustain this run. Those remain measured gates.

## Canonical artifacts

The Bazel graph publishes three operator entrypoints and the recorder/scorer
binary:

| Bazel target | Bazel output | Purpose |
|---|---|---|
| `//:build-gate0a-capture-stack` | `bazel-bin/build-gate0a-capture-stack` | Build the pinned upstream capture stack in a Crabbox |
| `//:capture-gate0a` | `bazel-bin/capture-gate0a` | Run the fail-closed wrapper on the physical analyzer host |
| `//:finalize-gate0a-capture` | `bazel-bin/finalize-gate0a-capture` | Hash and finalize one published capture in a Crabbox |
| `//:invisible-keyboard` | `bazel-bin/invisible-keyboard` | Exact `record-hand` binary on the physical host and byte-identical scorer in the Crabbox |

The first target emits these artifacts under its requested output directory:

| Artifact | Purpose |
|---|---|
| `artifacts/sigrok-cli-gate0a` | Pinned sigrok-cli with patched libsigrok linked into the executable |
| `artifacts/libsigrok-gate0a.patch` | Exact DSLogic fail-closed patch |
| `artifacts/gate0a-dslogic-plus-firmware.tar` | Deterministic two-blob firmware bundle plus manifest and DSView license |
| `artifacts/capture-stack-provenance.md` | Source, license, and firmware provenance |
| `corresponding-source/` | Pinned upstream source archives, patch, recipe, and license texts |
| `logs/` | Autotools configuration, compilation, version, file, and dynamic dependency evidence |
| `SHA256SUMS` | SHA-256 for every generated small artifact and build log |

One physical capture publishes these files in one directory. The handoff is
published last and is the commit marker for the complete set:

| Artifact | Authored where | Decisive content |
|---|---|---|
| `<name>.gate0a.raw` | Hardware host | Every 2 MHz `u16` sample word |
| `<name>.iks3` | Hardware host | Exact request, frames, receipt, and finalized IKS3 session |
| `<name>.locked-manifest.json` | Hardware host | Boot-locked, evidence-bound one-hand manifest |
| `<name>.capture.log` | Hardware host | Upstream log, firmware identities, driver receipt, supervision receipt |
| `<name>.recorder.log` | Hardware host | Bounded recorder output retained as diagnostic evidence, never a success oracle |
| `<name>.sigrok-cli` | Hardware host | Exact analyzer executable used |
| `<name>.invisible-keyboard` | Hardware host | Exact recorder binary used and required scorer binary |
| `<name>.libsigrok.patch` | Hardware host | Exact project patch used |
| `<name>.device-firmware.tar` | Hardware host | Exact firmware bytes offered to libsigrok |
| `<name>.acquisition-template.json` | Hardware host | Exact one-hand input template |
| `<name>.rig-evidence.json` | Hardware host | Exact as-built evidence supplied to admission |
| `<name>.axis-characterization.json` | Hardware host | Exact passed axis report supplied to admission |
| `<name>.handoff.json` | Hardware host | Settings, commands, stop observation, paths, sizes, and small-artifact identities |

Every failed path stays under `.<name><suffix>.partial`; the wrapper never
publishes a subset. The Crabbox finalizer adds caller-selected sidecar and
report files as one rollback-safe pair. The sidecar binds the raw hash and
capture provenance. The report binds the manifest, IKS3, raw, sidecar, and
derived terminal metrics. Neither file accepts recorder stdout as evidence.

## Version pins

| Component | Revision |
|---|---|
| [libsigrok](https://github.com/sigrokproject/libsigrok/commit/0bc2487778e660f4d3116729b6f4aee2b1996bb0) | `0bc2487778e660f4d3116729b6f4aee2b1996bb0` |
| [sigrok-cli](https://github.com/sigrokproject/sigrok-cli/commit/f44dd91347e7ac797cefc23162b9fcf0b7329f1f) | `f44dd91347e7ac797cefc23162b9fcf0b7329f1f` |
| [sigrok-firmware](https://github.com/sigrokproject/sigrok-firmware/commit/11eed913a9c535b87b5d5b5b92d2622cf34cee8b) | `11eed913a9c535b87b5d5b5b92d2622cf34cee8b` |
| [sigrok-util DSLogic extractor](https://github.com/sigrokproject/sigrok-util/commit/69747ca48b3d17ac7532439e487b9ba829828cfd) | `69747ca48b3d17ac7532439e487b9ba829828cfd` |
| [DSView firmware source snapshot](https://github.com/DreamSourceLab/DSView/commit/886b847c21c606df3138ce7ad8f8e8c363ee758b) | `886b847c21c606df3138ce7ad8f8e8c363ee758b` |

The pinned sigrok-firmware collection does not contain DSLogic firmware. The
build follows the pinned sigrok-util recipe, downloads only `DSLogicPlus.bin`
and `DSLogicPlus.fw` from the named DSView commit, checks fixed byte counts and
SHA-256 values, and places them in one normalized tar. The capture wrapper
offers only that extracted directory first and rejects a run unless the log
shows both exact files opened from it.

## Analyzer and channel contract

Use one DSLogic Plus with ten probes and a common ground. The upstream driver
identifies the model as USB VID:PID `2a0e:0020` and exposes 16 logic channels
in its [pinned device table](https://github.com/sigrokproject/libsigrok/blob/0bc2487778e660f4d3116729b6f4aee2b1996bb0/src/hardware/dreamsourcelab-dslogic/api.c#L21-L46).

| Analyzer bit | Rev A signal |
|---:|---|
| 0 | Sensor 0 INT1 |
| 1 | Sensor 1 INT1 |
| 2 | Sensor 2 INT1 |
| 3 | Sensor 3 INT1 |
| 4 | Sensor 4 INT1 |
| 5 | Sensor 5 INT1 |
| 6 | D8 snapshot marker |
| 7 | D9 cache-commit marker |
| 8 | SDA |
| 9 | SCL |
| 10 through 15 | Must remain zero |

Connect the analyzer ground to the same Rev A digital ground used by the
controller and sensors. Do not use ten independent ground paths. Keep SDA and
SCL probes short. The 20 MHz preflight can reveal extra threshold crossings,
but a logic analyzer cannot establish whether a probe caused analog ringing.

The full run uses the driver option `voltage_threshold=1.4-3.6`. On DSLogic
Plus, the pinned driver programs the midpoint, 2.5 V, through its
[voltage-threshold path](https://github.com/sigrokproject/libsigrok/blob/0bc2487778e660f4d3116729b6f4aee2b1996bb0/src/hardware/dreamsourcelab-dslogic/api.c#L462-L499).
Treat that as a starting capture setting, not proof of electrical margin. The
20 MHz preflight checks only the thresholded digital result with the real
probes, pull-ups, cable lengths, and powered hardware. An oscilloscope and the
verified limits for every connected receiver are still required to establish
high/low voltage margin, ringing, overshoot, or undershoot.

## Why 2 MHz and 7,220,000,000 samples

Two megahertz is an explicit supported rate in the pinned
[DSLogic rate table](https://github.com/sigrokproject/libsigrok/blob/0bc2487778e660f4d3116729b6f4aee2b1996bb0/src/hardware/dreamsourcelab-dslogic/api.c#L88-L105).
It provides 0.5 us timepoints, five timepoints per nominal 400 kHz I2C period,
and 150 timepoints across a 75 us INT1 pulse.

Keep these units separate:

| Quantity | Timepoints or words | Raw bytes |
|---|---:|---:|
| Minimum admitted 3,601-second window | 7,202,000,000 | 14,404,000,000 |
| Requested 3,610-second capture | 7,220,000,000 | 14,440,000,000 |

`--samples 7220000000` means timepoints, not bytes. The DSLogic driver
deinterleaves every sample into a `uint16_t`, and the raw binary module copies
those bytes without a header or transformation. See the pinned
[deinterleaver and datafeed path](https://github.com/sigrokproject/libsigrok/blob/0bc2487778e660f4d3116729b6f4aee2b1996bb0/src/hardware/dreamsourcelab-dslogic/protocol.c#L728-L874)
and [binary output module](https://github.com/sigrokproject/libsigrok/blob/0bc2487778e660f4d3116729b6f4aee2b1996bb0/src/output/binary.c).

## Patch behavior

The upstream DSLogic context stores `sent_samples` in a 32-bit `unsigned int`.
At 2 MHz it wraps after about 2,147 seconds, before this run ends. The project
patch changes the field and all per-capture evidence counters to `uint64_t`.

Upstream also accepts a timed-out transfer that returned some bytes, retries
some empty or failed transfers, and only logs an invalid DSLogic sample-block
length. Any one of those behaviors can remove or misalign time without an
explicit gap. The patch stops acquisition on the first:

- USB status other than `LIBUSB_TRANSFER_COMPLETED`;
- zero-byte transfer;
- byte count that is not divisible by `8 * enabled_channel_count`;
- failed USB transfer resubmission; or
- malformed or non-completed trigger-position transfer.

The driver increments `completed_transfers` once after each validated USB block
contributes sample words. Finalization emits
`IKB_DSLOGIC_TRANSFER_V1 completed_transfers=N anomalies=0` only if the exact
requested sample limit was emitted, `N` is positive, and no anomaly occurred.

The trigger transfer and continuous data transfers do not overlap in the
pinned control flow. `dslogic_acquisition_start` tracks the trigger request as
the sole transfer. A valid `trigger_receive` callback consumes it and calls
`start_transfers`, which resets the submitted count, frees and replaces the
trigger pointer array, and submits the data transfers. The callback then frees
the trigger request directly. Exact-limit teardown can therefore cancel only
data transfers. Those peer callbacks see `acq_aborted` before the status check,
free their unique slots without adding anomalies, and the final decrement to
zero calls `finish_acquisition` once. This is why treating a cancelled trigger
request as an anomaly does not reject normal sample-limit teardown. The exact
changes and surrounding control flow are reviewable in
`third_party/libsigrok-0bc2487-dslogic-gate0a.patch` and the pinned
[protocol source](https://github.com/sigrokproject/libsigrok/blob/0bc2487778e660f4d3116729b6f4aee2b1996bb0/src/hardware/dreamsourcelab-dslogic/protocol.c).

Pinned sigrok-cli returns zero from `main` even after many logged failures. See
the [pinned main function](https://github.com/sigrokproject/sigrok-cli/blob/f44dd91347e7ac797cefc23162b9fcf0b7329f1f/main.c#L231-L340).
The wrapper therefore requires both a zero process status and the internal
driver receipt. A zero status alone is never evidence.

## Build in a Crabbox

All package installation, patch application, compilation, and tests belong in
the Crabbox. Do not run these commands on the hardware host or local workspace.
The frozen repository path is `/home/user/invisible-keyboard` everywhere in
this guide. Do not substitute a second repository path during handoff.

Install build prerequisites in the active Ubuntu 24.04 x86-64 Crabbox:

```bash
BOX_ID="<active-crabbox-id>"
box exec --timeout 300 "$BOX_ID" -- \
  "sudo apt-get update -qq && sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq autoconf automake libtool pkg-config make gcc g++ libglib2.0-dev libzip-dev libbz2-dev libssl-dev libtirpc-dev libusb-1.0-0-dev libserialport-dev libftdi1-dev libhidapi-dev libbluetooth-dev libudev-dev python3 swig check zlib1g-dev"
```

`libbz2-dev` and `libssl-dev` supply libraries named by the static pkg-config
link flags. Runtime libraries alone do not provide the linker inputs for
`-lbz2`, `-lssl`, and `-lcrypto`.
`libtirpc-dev` preserves the canonical RPC feature set and `libtirpc.so.3`
dependency. Omitting it changes both pinned build artifacts even when the
DSLogic-only build succeeds.

Copy the frozen repository into an isolated Box directory. Bazel is the only
supported build entrypoint, so keep `BUILD.bazel`, `MODULE.bazel`, and the
declared runfiles with the capture-stack sources:

```bash
box exec --timeout 120 "$BOX_ID" -- \
  "test -f /home/user/invisible-keyboard/BUILD.bazel && test -f /home/user/invisible-keyboard/MODULE.bazel"
```

Run the build through its Bazel target. The target supplies the patch and
provenance paths from declared runfiles; the recipe does not infer a source
root from its staged executable path:

```bash
box exec --detach "$BOX_ID" -- \
  "cd /home/user/invisible-keyboard && IKB_CRABBOX_BUILD=1 bazelisk --batch run //:build-gate0a-capture-stack -- /home/user/gate0a-stack-build"
```

Poll the detached process with `box exec --status <pid> "$BOX_ID"`.

Build both Python operator entrypoints and the exact recorder/scorer through
Bazel. These are the only project executables admitted by the commands below:

```bash
box exec --timeout 120 "$BOX_ID" -- \
  "cd /home/user/invisible-keyboard && bazelisk --batch build //:capture-gate0a //:finalize-gate0a-capture //:invisible-keyboard"
```

After the build succeeds, run the focused Bazel contract with the real capture
binary and firmware tar explicitly forwarded into the test sandbox. This
exercises the canonical build entrypoint and the full sparse wrapper path. The
sparse fixture avoids allocating 14.44 GB of physical storage. The finalizer
contract still reads and hashes the full 14,440,000,000-byte logical stream:

```bash
box exec --timeout 120 "$BOX_ID" -- \
  "cd /home/user/invisible-keyboard && bazelisk --batch test --nocache_test_results --test_output=errors --test_env=IKB_GATE0A_TEST_CAPTURE_BINARY=/home/user/gate0a-stack-build/artifacts/sigrok-cli-gate0a --test_env=IKB_GATE0A_TEST_FIRMWARE_BUNDLE=/home/user/gate0a-stack-build/artifacts/gate0a-dslogic-plus-firmware.tar //:gate0a-capture-stack-test"
```

The build refuses a non-empty destination. It verifies the exact commits,
checks and applies the patch, performs out-of-tree upstream Autotools builds,
links libsigrok into sigrok-cli, rejects a remaining dynamic `libsigrok`
dependency, verifies the embedded receipt, creates the firmware tar, and emits
checksums and corresponding source.

The executable still uses normal system libraries such as glib, libusb, and
libzip. Build it in a Box whose Ubuntu release and architecture match the
physical capture host. Check `logs/sigrok-cli-ldd.log` before copying it.

## Box validation record

On 2026-08-29, Crabbox `bx_v97q4knd` built the stack twice from clean output
directories on Ubuntu 24.04 x86-64 with GCC 13.3. Both builds applied the patch
to the exact libsigrok commit and compiled libsigrok and sigrok-cli from source.
The output-directory-independent artifacts matched byte for byte:

| Artifact | SHA-256 in both builds |
|---|---|
| `sigrok-cli-gate0a` | `1665066921d1163bb2dddd45ae2ea20b55cf6970ac8de46ef5b0dbd2804e65c3` |
| `gate0a-dslogic-plus-firmware.tar` | `bc85f870076a05aaa82393f341b2fca6b33b22deda92bf873e4979f0d645f8de` |
| `libsigrok-gate0a.patch` | `55d354597a5573315a16408112cf4dfbbda8976842b428c42681dbbc986bd10d` |
| `BUILD-INFO.txt` | `0fcb2d7e6ebb9593173cbbbba74502a225111e3e0a2c97a684d5101f7c738268` |

The latest focused remote receipt exercised 21 no-mock capture-stack cases. It
includes patch and argv contracts, private pipe and stop-window supervision,
paired child failures, signal handling, log quotas, firmware identities,
immutable publication, same-binary scoring, finalizer rollback, the Bazel
runfiles entrypoint, and the full sparse-wrapper fixture. The final frozen-tree
validation report, not this historical focused receipt, owns the release pass
claim. Build logs and resolved
runtime libraries are under `/home/user/gate0a-stack-build-v5/logs/` in that
Box. No DSLogic Plus was attached, so this record does not claim a physical
capture or close any item in the residual trust boundary below.

## Physical preflight sequence

Do not start with the one-hour capture.

1. Verify at least 18,850,612,736 bytes of free space on the capture filesystem.
2. Wire all ten signals and the common ground.
3. Power-cycle the DSLogic Plus. Do not let DSView or another sigrok process
   preload firmware.

The free-space floor retains every raw artifact on one filesystem and reserves
an explicit operating allowance:

```text
   400,000,000  10-second 20 MHz preflight raw
 2,400,000,000  10-minute 2 MHz soak raw
14,440,000,000  decisive 3,610-second raw
   536,870,912  wrapper's mandatory 512 MiB free margin
 1,073,741,824  preflight/capture logs, five small companion files, temporary firmware, and filesystem metadata
--------------
18,850,612,736  required free bytes before the first preflight
```

The 1 GiB operating allowance is a field budget, not a proved log-size bound.
If preflight logs and small artifacts consume it, stop and diagnose the excess
instead of deleting evidence. The capture wrapper independently rechecks its
14,440,000,000 + 536,870,912 byte floor immediately before the decisive run.

Create all retained outputs below one filesystem and perform the exact byte
check before creating the preflight directory:

```bash
REPO_ROOT=/home/user/invisible-keyboard
STACK_ROOT=/home/user/gate0a-stack-build
CAPTURE_ROOT=/data/invisible-keyboard
PREFLIGHT_ROOT="$CAPTURE_ROOT/gate0a-preflight"
FULL_CAPTURE_DIR="$CAPTURE_ROOT/gate0a"
REQUIRED_FREE_BYTES=18850612736

test -d "$REPO_ROOT"
test -x "$REPO_ROOT/bazel-bin/capture-gate0a"
test -x "$REPO_ROOT/bazel-bin/invisible-keyboard"
test -x "$STACK_ROOT/artifacts/sigrok-cli-gate0a"
test -f "$STACK_ROOT/artifacts/libsigrok-gate0a.patch"
test -f "$STACK_ROOT/artifacts/gate0a-dslogic-plus-firmware.tar"
(cd "$STACK_ROOT" && sha256sum -c SHA256SUMS)
mkdir -p "$CAPTURE_ROOT" "$FULL_CAPTURE_DIR"
AVAILABLE_BYTES=$(($(stat -f -c '%a' "$CAPTURE_ROOT") * $(stat -f -c '%S' "$CAPTURE_ROOT")))
test "$AVAILABLE_BYTES" -ge "$REQUIRED_FREE_BYTES"
test ! -e "$PREFLIGHT_ROOT"
```

Stage the already hash-checked firmware bundle in a new directory. Both
preflight commands below inherit this exact resource path:

```bash
FIRMWARE_STAGE="$PREFLIGHT_ROOT/firmware"
mkdir -p "$FIRMWARE_STAGE" "$FIRMWARE_STAGE/xdg-home-empty" \
  "$FIRMWARE_STAGE/xdg-system-empty"
tar -xf "$STACK_ROOT/artifacts/gate0a-dslogic-plus-firmware.tar" \
  -C "$FIRMWARE_STAGE" \
  dreamsourcelab-dslogic-plus-fpga.fw \
  dreamsourcelab-dslogic-plus-fx2.fw
test "$(sha256sum "$FIRMWARE_STAGE/dreamsourcelab-dslogic-plus-fpga.fw" | cut -d' ' -f1)" = \
  e16c70e4af9181044e3ec3a363e4bb109f1d731b4f9f046f8bc1d1015a7842a7
test "$(sha256sum "$FIRMWARE_STAGE/dreamsourcelab-dslogic-plus-fx2.fw" | cut -d' ' -f1)" = \
  9197e08dd33a9cda61cc8cd3de559cbae9b049b2dfa827c089ffe0528f5dcc5e
export SIGROK_FIRMWARE_DIR="$FIRMWARE_STAGE"
export SIGROK_FIRMWARE_PATH=""
export XDG_DATA_HOME="$FIRMWARE_STAGE/xdg-home-empty"
export XDG_DATA_DIRS="$FIRMWARE_STAGE/xdg-system-empty"
```

### Ten-second 20 MHz thresholded-digital preflight

This command requests exactly 200,000,000 sample words and must produce
400,000,000 raw bytes. Hold the Rev A controller in reset, then run this block
in the analyzer-hand terminal:

```bash
(
  set -euo pipefail
  RAW="$PREFLIGHT_ROOT/preflight-20mhz.raw"
  LOG="$PREFLIGHT_ROOT/preflight-20mhz.log"
  test ! -e "$RAW" && test ! -e "$LOG"
  "$STACK_ROOT/artifacts/sigrok-cli-gate0a" \
    --driver dreamsourcelab-dslogic \
    --channels 0,1,2,3,4,5,6,7,8,9 \
    --config samplerate=20m:voltage_threshold=1.4-3.6:continuous=true \
    --samples 200000000 \
    --output-format binary \
    --output-file "$RAW" \
    --loglevel 4 \
    2>"$LOG"
  test "$(stat -c '%s' "$RAW")" -eq 400000000
  test "$(grep -Ec 'IKB_DSLOGIC_TRANSFER_V1 completed_transfers=[1-9][0-9]* anomalies=0$' "$LOG")" -eq 1
  ! grep -Eq 'IKB_DSLOGIC_TRANSFER_FAILURE_V1|Capture rejected|CRITICAL|WARNING' "$LOG"
)
```

In the controller-hand terminal, release reset only after the raw file begins
and before 40,000,000 bytes, which is one second at 20 MHz:

```bash
PREFLIGHT_ROOT=/data/invisible-keyboard/gate0a-preflight
PREFLIGHT_RAW="$PREFLIGHT_ROOT/preflight-20mhz.raw"
while test ! -s "$PREFLIGHT_RAW"; do
  sleep 0.05
done
PREFLIGHT_BYTES=$(stat -c '%s' "$PREFLIGHT_RAW")
test "$PREFLIGHT_BYTES" -le 40000000
printf 'release controller reset now; raw_bytes=%s\n' "$PREFLIGHT_BYTES"
```

The released Rev A firmware, six physical sensor INT1 outputs, D8/D9 marker
GPIOs, and the muxed I2C bus are the exact waveform source. The channel table
above is the wiring map. Missing the one-second release deadline rejects this
preflight and its boot-prefix inspection.

Import the exact two-byte words as 16 thresholded logic channels. PulseView's
[command-line interface](https://sigrok.org/doc/pulseview/0.4.2/manual.html#_command_line_interface)
defines `-i` and `-I`; the pinned libsigrok
[binary input module](https://github.com/sigrokproject/libsigrok/blob/0bc2487778e660f4d3116729b6f4aee2b1996bb0/src/input/binary.c#L35-L99)
maps 16 channels to a two-byte sample unit and accepts the explicit sample
rate:

```bash
pulseview -c \
  -i "$PREFLIGHT_ROOT/preflight-20mhz.raw" \
  -I binary:numchannels=16:samplerate=20000000
```

Label D0 through D9 from the channel table and retain D10 through D15 in the
view. At 20 MHz one sample is 50 ns. The thresholded digital preflight passes
only when all of these are true in the imported artifact:

- D10 through D15 remain zero for the full file;
- every intended edge appears as one transition, with no extra 50 ns or longer
  threshold crossing around it;
- the boot prefix is D8 high for 20 to 200 us, both markers low for at least
  20 us, D9 high for 20 to 200 us, then both low for at least 20 us, with no
  INT1 or I2C activity in that prefix;
- later D8 and D9 high intervals are at least 2 us and less than 20 us, and the
  two markers never overlap;
- each approximately 75 us INT1 pulse is one high interval rather than a burst
  of threshold crossings; and
- SDA and SCL decode as the expected I2C traffic without an extra threshold
  crossing, missing edge, or sub-sample pulse.

In PulseView, add the I2C protocol decoder, bind its SDA input to analyzer bit 8
and SCL input to bit 9, then inspect both the decoded rows and the underlying
edges. The decoder is an inspection aid only. Its transaction list is not
decisive Gate 0A evidence.

This procedure checks only the 2.5 V comparator result sampled every 50 ns. It
cannot observe a shorter excursion, high/low voltage margin, rise/fall shape,
ringing that does not recross 2.5 V, overshoot, or undershoot. Do not call those
analog properties passed from this file. Close them with an oscilloscope at the
real controller and sensor pins, using verified `VIL`, `VIH`, and absolute
maximum limits from the exact populated component revisions and the exact
pull-up BOM. Those values are intentionally not invented here. Until that
scope record exists, analog signal integrity remains an open physical gate.

The repository publishes no deterministic signal-generator firmware, wiring
fixture, or Bazel target. Therefore this revision does not require or claim a
signal-generator preflight. Adding one later requires a hash-bound Bazel
artifact, exact pin map, waveform table, and machine-checked transition
counts. The actual Rev A controller and sensor assembly is the waveform source
for the two preflights in this guide.

This capture is non-decisive and does not need a raw hash or sidecar. Retain
both its raw file and log.

### Ten-minute 2 MHz transport soak

After the electrical check passes, request exactly 1,200,000,000 sample words.
The raw file must be 2,400,000,000 bytes:

```bash
(
  set -euo pipefail
  RAW="$PREFLIGHT_ROOT/soak-2mhz.raw"
  LOG="$PREFLIGHT_ROOT/soak-2mhz.log"
  test ! -e "$RAW" && test ! -e "$LOG"
  "$STACK_ROOT/artifacts/sigrok-cli-gate0a" \
    --driver dreamsourcelab-dslogic \
    --channels 0,1,2,3,4,5,6,7,8,9 \
    --config samplerate=2m:voltage_threshold=1.4-3.6:continuous=true \
    --samples 1200000000 \
    --output-format binary \
    --output-file "$RAW" \
    --loglevel 4 \
    2>"$LOG"
  test "$(stat -c '%s' "$RAW")" -eq 2400000000
  test "$(grep -Ec 'IKB_DSLOGIC_TRANSFER_V1 completed_transfers=[1-9][0-9]* anomalies=0$' "$LOG")" -eq 1
  ! grep -Eq 'IKB_DSLOGIC_TRANSFER_FAILURE_V1|Capture rejected|CRITICAL|WARNING' "$LOG"
)
```

This soak is also non-decisive and does not need a raw hash or sidecar. A short
file, missing or duplicate receipt, or failure marker rejects it. After it
passes, retain both files and power-cycle the analyzer again.

The wrapper itself runs a fast `--show` setup first. This forces both staged
firmware files to load and fails before the hour-long run if the analyzer was
not power-cycled. The capture process then reopens the staged FPGA bitstream.
The log must show the FX2 file once and the FPGA file twice.

## Deterministic quiesce handoff

The capture wrapper now owns both sigrok and the one-hand duplex recorder. It
creates two private pipes, starts both children, and treats neither child
stdout nor operator timing as a success oracle. The recorder publishes one
readiness byte only after controller identity is locked and the IKS3 header
exists. It opens the controller as one exclusive duplex TTY in raw 921600 8N1
mode. The wrapper sends one private stop byte at the exact raw-size window
and keeps that pipe open as the analyzer-success guard.

While reset is held, the recorder waits at most three seconds for the declared
USB path to appear. Other open errors fail immediately, and a later disconnect
never reopens the device. Readiness must arrive by 12,000,000 raw bytes.

The resulting hard cut is one canonical sequence: one outbound `IKQ2` request,
zero or more ordinary frames, one clean scheduled final D8 frame `N`, the exact
52-transaction and 1,422-SCL-rise shutdown suffix, one atomic D8+D9 terminal
marker, one matching `IKR2` receipt, and silence through analyzer EOF plus the
host's final 100 ms TTY guard. Signals, reset, power-off, manual trimming, an
IKS2 session, or a separate receipt file are failures, not substitutes.

[`gate0a-quiesce-contract.md`](gate0a-quiesce-contract.md) is the one
authoritative byte, state, waveform, deadline, and failure contract. This guide
defines only the operator timeline and artifact handoff.

Raw size is the coordination clock because the capture emits 4,000,000 bytes
per second:

| Owner | Raw byte deadline | Capture time | Required action |
|---|---:|---:|---|
| Wrapper | 0 | 0 s | Start sigrok and the Bazel-built recorder with the controller held in reset |
| Controller operator | at most 4,000,000 | at most 1 s | Release reset so the complete boot prefix is inside the raw file |
| Recorder | at most 12,000,000 | at most 3 s | Lock identity, create IKS3, and publish readiness early enough for the 3,601-second scored overlap |
| Wrapper | 14,416,000,000 through 14,417,000,000 | first observation at 3,604.000 through 3,604.250 s | Send exactly one private stop byte |
| Firmware | marker fall no later than sample 7,210,000,000 | at most 3,605 s | Close on one final D8, shut down all sensors, emit the atomic marker, then offer the receipt |
| Analyzer | 14,440,000,000 | 3,610 s | Reach exact EOF with at least 10,000,000 idle samples after marker fall |
| Recorder | after analyzer-success EOF | at least 3,610.100 s | Observe a further 100 ms of TTY silence, then attempt `END3`, file/parent sync, and strict audit |

The wrapper polls raw size every 1 ms and rejects any observed poll gap over
10 ms. The first eligible stop observation must not exceed 14,417,000,000
bytes. It closes the guard pipe only after sigrok exits zero, the raw file is
exactly 14,440,000,000 bytes and synced, and the final analyzer log passes its
quota, failure-marker, driver-receipt, and firmware-open-count checks. A rejected
log aborts the recorder before cleanup can close the guard. Missing any deadline,
extra device bytes, or either child failing suppresses publication. The
operator must not shorten the active interval, extend the analyzer capture,
trim either evidence file, or substitute manual timestamps.

Every protocol, receipt, signal, disconnect, timeout, post-receipt-byte, or
analyzer-guard failure before footer creation leaves no `END3`. A later file or
parent sync failure, or an I/O failure during strict post-footer audit, can
leave `END3` bytes in the hidden session partial. The recorder still exits
nonzero and the wrapper publishes nothing. Footer bytes alone are never a
capture success oracle.

## Physical-host capture

The only non-Crabbox execution is the acquisition that needs the physical USB
analyzer and controller TTY. Copy the Bazel-published
`bazel-bin/capture-gate0a` and `bazel-bin/invisible-keyboard`, generated sigrok
capture executable, patch, firmware tar, `SHA256SUMS`, and the frozen one-hand
template/rig/axis inputs to the compatible Linux host. Place the frozen
repository at `/home/user/invisible-keyboard` and the stack at
`/home/user/gate0a-stack-build`, the same paths used in the Crabbox commands.
Verify the small-file checksums. Do not rebuild, lint, test, hash the raw file,
or score on the hardware host.

Create the output directory on the preflighted filesystem, hold the controller
in reset, and run the single supervising wrapper. Use the exact one-hand
template, rig evidence, axis characterization, and stable TTY path selected for
this capture:

```bash
REPO_ROOT=/home/user/invisible-keyboard
STACK_ROOT=/home/user/gate0a-stack-build
FULL_CAPTURE_DIR=/data/invisible-keyboard/gate0a
CAPTURE_NAME=left-rev-a-001
TEMPLATE=/home/user/evidence/gate0a-left-template.json
RIG_EVIDENCE=/home/user/evidence/gate0a-left-rig-evidence.json
AXIS_CHARACTERIZATION=/home/user/evidence/gate0a-left-axis-characterization.json
DEVICE=/dev/serial/by-id/REPLACE_WITH_STABLE_CONTROLLER_ID

test -x "$REPO_ROOT/bazel-bin/capture-gate0a"
test -x "$REPO_ROOT/bazel-bin/invisible-keyboard"
test -x "$STACK_ROOT/artifacts/sigrok-cli-gate0a"
test -f "$TEMPLATE" && test -f "$RIG_EVIDENCE" && test -f "$AXIS_CHARACTERIZATION"
mkdir -p "$FULL_CAPTURE_DIR"

"$REPO_ROOT/bazel-bin/capture-gate0a" \
  --sigrok-cli "$STACK_ROOT/artifacts/sigrok-cli-gate0a" \
  --recorder "$REPO_ROOT/bazel-bin/invisible-keyboard" \
  --driver-patch "$STACK_ROOT/artifacts/libsigrok-gate0a.patch" \
  --firmware-bundle "$STACK_ROOT/artifacts/gate0a-dslogic-plus-firmware.tar" \
  --template "$TEMPLATE" \
  --rig-evidence "$RIG_EVIDENCE" \
  --axis-characterization "$AXIS_CHARACTERIZATION" \
  --device "$DEVICE" \
  --output-directory "$FULL_CAPTURE_DIR" \
  --name "$CAPTURE_NAME"
```

In the controller-hand terminal, wait for the actual raw file to begin and
enforce the one-second boot deadline before releasing reset:

```bash
FULL_CAPTURE_DIR=/data/invisible-keyboard/gate0a
CAPTURE_NAME=left-rev-a-001
RAW_PARTIAL="$FULL_CAPTURE_DIR/.$CAPTURE_NAME.gate0a.raw.partial"
while test ! -s "$RAW_PARTIAL"; do
  sleep 0.05
done
RAW_BYTES=$(stat -c '%s' "$RAW_PARTIAL")
test "$RAW_BYTES" -le 4000000
printf 'release controller reset now; raw_bytes=%s\n' "$RAW_BYTES"
```

The wrapper starts the recorder itself. Its child interface is exactly:

```text
record-hand <template> <rig-evidence> <axis-characterization> <device>
  <locked-manifest.partial> <session.partial>
  --ready-fd <fd> --stop-fd <fd>
```

The two descriptors are private inherited pipes. The recorder writes one
`0x01` readiness byte and closes that writer. The wrapper later writes one
`0x01` stop byte but keeps its writer open through analyzer completion. No
operator opens either descriptor, sends the request, or closes the guard.
Recorder output goes only to the bounded `<name>.recorder.log`; it is not a
readiness, quiesce, or finalization oracle.

The child capture argv is fixed to:

```text
sigrok-cli-gate0a
  --driver dreamsourcelab-dslogic
  --channels 0,1,2,3,4,5,6,7,8,9
  --config samplerate=2m:voltage_threshold=1.4-3.6:continuous=true
  --samples 7220000000
  --output-format binary
  --output-file <exclusive-temporary-raw-path>
  --loglevel 4
```

The displayed analyzer and recorder executable names are shorthand. The
wrapper actually executes exclusive hidden copies named
`.<name>.sigrok-cli.partial` and `.<name>.invisible-keyboard.partial`. The
handoff preserves both actual command arrays and binds the corresponding
published basenames. The finalizer checks those exact relations. It does not
claim that the temporary command path and published path string are equal.

The wrapper requires little-endian 64-bit Linux. It reserves temporary names,
refuses every existing final or partial path, checks free space, copies and
hashes only the small artifacts, validates and extracts the fixed firmware
tar, runs sigrok-cli without a shell, and records stderr verbatim.
The setup process has a 60-second deadline. The full process has a
3,910-second deadline, which is the nominal 3,610-second sample interval plus
five minutes for startup and teardown. A deadline breach terminates the child
process group, suppresses the success receipt, and leaves partial evidence.

On success it:

1. finds exactly one positive patched-driver receipt;
2. finds both exact staged firmware paths in the expected setup and capture
   counts;
3. rejects any transfer-failure marker, warning, critical line, child timeout,
   signal, or log-quota breach;
4. requires zero status from both children, exactly 14,440,000,000 raw bytes,
   and a structurally finalized IKS3 session;
5. syncs raw, IKS3, locked manifest, and both logs;
6. appends the two loaded-firmware SHA-256 lines and exactly one
   `IKB_GATE0_CAPTURE_V2` supervision receipt with sample count, transfer
   count, stop-observation bytes, IKS3 size, locked-manifest hash, and firmware
   bundle hash;
7. copies and hashes the exact binaries and bounded evidence inputs; and
8. publishes the complete artifact set without overwrite, with the handoff
   last.

The capture log has one accepted supervision format. Its final line is
LF-terminated and has this exact field order:

```text
IKB_GATE0_CAPTURE_V2 samples=<requested> completed_transfers=<positive-u64> anomalies=0 stop_observed_raw_bytes=<14416000000..=14417000000> session_bytes=<exact-finalized-IKS3-byte-length> locked_manifest_sha256=<64-lowercase-hex> firmware_bundle_sha256=<64-lowercase-hex>
```

The finalizer rejects V1, extra, missing, or reordered fields, a stop outside
the inclusive range, a nonpositive or mismatched transfer count, any trailing
token, and session length, manifest hash, or firmware bundle hash that does not
match the published evidence.

The hardware wrapper intentionally does not hash the 14.44 GB raw file. It
also does not score, decode terminal marker indices, or claim a pass. Those
resource-heavy operations remain Crabbox-only. The handoff is therefore not a
scorer sidecar or report.

On failure, the wrapper exits nonzero and leaves hidden `.partial` artifacts.
Do not delete them automatically. Inspect and move them before retrying. This
prevents a retry from concealing the first failure.

## Crabbox finalization

Copy the entire published capture directory to the active Crabbox. Do not copy
only the raw file. The finalizer requires the handoff and every bound companion
artifact before it can create the sidecar and report.

```bash
box exec --timeout 120 "$BOX_ID" -- \
  "mkdir -p /home/user/gate0a-captures/left-rev-a-001"
box scp --recursive /data/invisible-keyboard/gate0a/. \
  "${BOX_ID}:/home/user/gate0a-captures/left-rev-a-001/"
```

Hash, reduce, score, and finalize through the Bazel-published target in the
same frozen repository used to build the capture stack. The scorer operand is
the wrapper-published recorder binary, not a newly built substitute:

```bash
box exec --detach "$BOX_ID" -- \
  "cd /home/user/invisible-keyboard && IKB_CRABBOX_FINALIZE=1 bazelisk --batch run //:finalize-gate0a-capture -- --handoff /home/user/gate0a-captures/left-rev-a-001/left-rev-a-001.handoff.json --sidecar-output /home/user/gate0a-captures/left-rev-a-001/left-rev-a-001.sidecar.json --report-output /home/user/gate0a-captures/left-rev-a-001/left-rev-a-001.report.json --scorer /home/user/gate0a-captures/left-rev-a-001/left-rev-a-001.invisible-keyboard"
```

The finalizer revalidates every setting, both child command arrays, path, size,
small-file hash, driver receipt, firmware identity, IKS3 envelope, stop
observation, and final capture-log line. It rejects the scorer unless its bytes
have the same SHA-256 as `<name>.invisible-keyboard`. Before the large-file
hashes, it copies the scorer from a no-follow descriptor into a sealed anonymous
Linux memory file and hashes that sealed image. It executes the retained image
through its inherited `/proc/self/fd` descriptor, so renaming, replacing, or
editing the scorer path cannot change the admitted bytes. It then hashes the
full IKS3 and 14.44 GB raw files, writes the bounded scorer sidecar, invokes the
retained binary's `score-gate0a` command exactly once, validates the canonical
report and terminal metrics, and publishes sidecar and report together. It
refuses symlinks, unsafe relative paths, schema extras, overwrite, partial pair
publication, and scorer stdout as evidence.

The scorer receives the locked manifest, rig evidence, axis characterization,
IKS3 session, raw capture, and generated sidecar. It does not receive
`handoff.json` as decisive waveform evidence. The finalizer uses the handoff to
bind and locate the complete capture set.

## Sidecar content

The generated sidecar has this shape:

```json
{
  "schema": "invisible-keyboard-gate0-dslogic-capture-v1",
  "raw_capture_sha256": "<64 lowercase hex characters>",
  "raw_capture_bytes": 14440000000,
  "word_bytes": 2,
  "byte_order": "little",
  "sample_rate_hz": 2000000,
  "channel_map": {
    "int1": [0, 1, 2, 3, 4, 5],
    "d8": 6,
    "d9": 7,
    "sda": 8,
    "scl": 9
  },
  "voltage_threshold_low_mv": 1400,
  "voltage_threshold_high_mv": 3600,
  "requested_samples": 7220000000,
  "capture_started_unix_ms": 1,
  "capture_finished_unix_ms": 2,
  "capture_binary": {"path": "...", "sha256": "...", "bytes": 1},
  "capture_log": {"path": "...", "sha256": "...", "bytes": 1},
  "device_firmware_bundle": {"path": "...", "sha256": "...", "bytes": 1},
  "driver_base_commit": "0bc2487778e660f4d3116729b6f4aee2b1996bb0",
  "sigrok_cli_base_commit": "f44dd91347e7ac797cefc23162b9fcf0b7329f1f",
  "device_model": "DSLogic Plus",
  "driver_patch_sha256": "<64 lowercase hex characters>",
  "capture_exit_status": 0,
  "command": ["..."]
}
```

The `bytes: 1` values above mean positive byte counts, not literal required
sizes. The finalizer writes their actual sizes.

The paired report uses schema `invisible-keyboard-gate0a-report-v6`. Its source
hashes bind the locked manifest, IKS3 session, raw capture, and sidecar. Its
`deterministic_quiesce` criterion uses the unchanged trace metrics
`final_sequence`, `shutdown_unadmitted_int1_edges`, `shutdown_transactions`,
`shutdown_scl_rising_edges`, `terminal_marker_rise_sample`,
`terminal_marker_fall_sample`, `terminal_marker_width_samples`,
`terminal_idle_samples`, `host_response_time_ns`, and
`deterministic_quiesce_violations`. The finalizer validates the exact types,
ordering, and pass boundaries before publishing either output.

## Failure matrix

| Observation | Result | Recovery |
|---|---|---|
| Build commit or firmware hash differs | Build fails | Inspect source URL, network interception, or changed local file; do not update a pin during a run |
| Built executable links `libsigrok.so` | Build fails | Use the static libsigrok recipe so the executable hash binds the patch |
| Frozen-tree quiesce, recorder, capture-wrapper, finalizer, or scorer contract fails | Decisive capture is blocked | Repair and rerun the single canonical path in a Crabbox; do not substitute Ctrl-C, reset, power-off, or another binary |
| Analyzer was not power-cycled | Setup fails before full capture | Stop other analyzer software, power-cycle, retain the partial log, retry with a new name or cleared reviewed directory |
| USB status is timeout, cancelled, stalled, overflowed, or another non-completed status | Driver stops immediately; no success receipt | Treat as failed physical Gate 0A evidence |
| Empty or incomplete DSLogic block | Driver stops immediately; no success receipt | Treat as failed physical Gate 0A evidence |
| Transfer resubmission fails | Driver stops immediately; no success receipt | Treat as failed physical Gate 0A evidence |
| Setup exceeds 60 seconds or capture exceeds 3,910 seconds | Wrapper terminates the child process group; no success receipt | Retain the partial log and diagnose firmware, USB, or storage before a new run |
| sigrok-cli returns zero but receipt is absent | Wrapper fails | Inspect the hash-bound log; never accept process status alone |
| Raw size is not 14,440,000,000 bytes | Wrapper fails | Retain partial raw and log; diagnose storage, USB, or process termination |
| Recorder exits before analyzer-success EOF, emits post-receipt bytes, or leaves IKS3 without `END3` | Wrapper fails and publishes nothing | Retain all hidden partials and diagnose the controller/recorder path |
| Finalizer scorer hash differs from the captured recorder hash | Finalization fails before scoring | Use the published `<name>.invisible-keyboard`; never replace the capture-time implementation |
| Start, quiesce-request, terminal-receipt, or stop-marker byte deadline is missed | Run is rejected | Retain every artifact, diagnose coordination or quiesce latency, and take a new immutable capture |
| Activity appears after the stop marker or a host frame follows the terminal sequence | Gate 0A fails | Fix the firmware/host stop contract; do not trim the raw capture or session |
| Any final artifact or partial name exists | Wrapper or finalizer fails before overwrite | Inspect and move the prior evidence explicitly |
| Raw changes during transfer or finalization | Sidecar size or later hash validation fails | Repeat transfer from the published hardware artifact; never edit raw bytes |
| Scorer finds reserved bits, malformed I2C, missing anchors, missing markers, bad accounting, or inadequate coverage | Gate 0A fails or is inconclusive | Fix the physical or firmware cause and take a new capture |

## GPL and firmware provenance

libsigrok, sigrok-cli, and the changed DSLogic driver are GPL software. The
build output places the exact upstream source archives, patch, build recipe,
notices, and license texts beside the executable. Distribute that
`corresponding-source/` directory with any executable copy.

The DSView repository carries GPL-3.0 `COPYING`, while the historical firmware
blobs are not reproducibly tied to the available
[DSLogic-fw](https://github.com/DreamSourceLab/DSLogic-fw) and
[DSLogic-hdl](https://github.com/DreamSourceLab/DSLogic-hdl) source snapshots.
The capture path fixes byte identity but does not resolve that source mapping.
Complete legal and supply-chain review before redistributing the firmware
bundle outside the development team.

The full notice and exact firmware hashes are in
`third_party/gate0a-capture-stack-provenance.md`.

## Residual physical trust boundary

After sidecar validation, the remaining boundary is narrow but real:

- DSLogic Plus hardware and its sample clock;
- ten probes, the common ground, and threshold behavior;
- the two staged firmware binaries and their not-yet-reproduced source mapping;
- the physical USB host, storage stack, and power stability;
- transitions shorter than or aligned unfavorably to the 0.5 us sample grid;
- the controller and sensor electrical behavior during the full run; and
- the scorer implementation itself, covered separately by Bazel differential,
  adversarial, and full-capture tests.

The physical completion sequence is therefore fixed: pass the frozen-tree
hardware-independent validation in a Crabbox, run the 10-second 20 MHz
thresholded-digital preflight, collect separate analog scope evidence, run the
10-minute 2 MHz soak, take the full supervised 3,610-second capture, then hash,
reduce, score, and publish the sidecar/report pair in a Crabbox. The repository
publishes no signal-generator fixture, so this guide does not claim one. Any
anomaly requires a new immutable capture. No waiver or human-authored
aggregate can turn a failed run into a pass.
