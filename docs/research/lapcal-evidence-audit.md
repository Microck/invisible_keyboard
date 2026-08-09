# LapCal evidence audit

## Bottom line

LapCal proves that its authors designed and assembled a six-IMU ESP32 glove
prototype, defined a 34-slot topology and packed ordering, captured several
single-user labeled motion streams, explored offline tap classifiers, and
produced a small binary TensorFlow Lite model. It does not publish a complete
34-position logical key manifest and does not prove a working invisible
keyboard. No tracked artifact demonstrates the full path from live motion
through key identity to operating-system text, and no result measures words per
minute, character error rate, false activations per hour, end-to-end latency,
session-to-session robustness, or user-to-user transfer.

The strongest reusable result is the failure evidence. Eight tracked captures
yield 14.3 to 19.1 instrumented scan-boundary measurements per second for a
shared ESP32 reading six MPU6050 DMP streams. The author's separate "about 10Hz
at best" statement is in tension with those calculations, but the repository
does not show that they measure the same end-to-end boundary or configuration.
Label synchronization repeatedly failed. The later single-finger work avoided
those bottlenecks, but its raw datasets are untracked and its model results are
not reproducible from the repository.

Confidence in this conclusion is **high** for artifact presence, static source
defects, tracked-data counts, and recomputed timing. It is **medium** for the
physical prototype's behavior because only repository media and author notes
are available. It is **low** for any model quality claim whose source dataset or
training log is absent.

## Audit boundary

This audit used the local clone at `research/LapCal` without changing it. The
audited main tip is immutable commit
[`8638759168d84d32fa50ae461e053055a01c3dff`](https://github.com/xjjak/LapCal/tree/8638759168d84d32fa50ae461e053055a01c3dff).
Material remote refs are:

| Ref | Tip | Material content |
| --- | --- | --- |
| `main` | [`8638759168d84d32fa50ae461e053055a01c3dff`](https://github.com/xjjak/LapCal/tree/8638759168d84d32fa50ae461e053055a01c3dff) | Current CAD, electronics, ESP32 firmware, literate data analysis |
| `machine-learning` and `5-feature-neural-network-compression-for-embedded-hardware` | [`824dfd0710a93c4515c07399d40555dc31c59ffa`](https://github.com/xjjak/LapCal/tree/824dfd0710a93c4515c07399d40555dc31c59ffa) | The same commit, with CNN training source and two TFLite files |
| `63-timestamp-accuracy` | [`3bb72ecaf0cfb4715ea82d00b4df920f19775cbd`](https://github.com/xjjak/LapCal/tree/3bb72ecaf0cfb4715ea82d00b4df920f19775cbd) | Wired host/ESP32 timestamp sample and analysis |
| `64-sweep-data-collection-setup` | [`5ea2f437c47f900fd3a1ef2ff0a3c4cdabf726f8`](https://github.com/xjjak/LapCal/tree/5ea2f437c47f900fd3a1ef2ff0a3c4cdabf726f8) | One-sensor keyboard-label collection rig and dataset notes |
| `state-detection` | [`178d1d09abd32191bf8c4ede54623facc1e385c8`](https://github.com/xjjak/LapCal/tree/178d1d09abd32191bf8c4ede54623facc1e385c8) | Older raw six-sensor datasets, preparation code, click notebook, broken live-preview draft |
| `keylogger-tools` | [`320bd7c9b20e4729a6e0890d6ac004886f1f8dbd`](https://github.com/xjjak/LapCal/tree/320bd7c9b20e4729a6e0890d6ac004886f1f8dbd) | Fully ancestral to main; no unique evidence |

Each of the four material non-main refs has one tip commit outside main's
ancestry. The two machine-learning names are aliases, not independent results.
The branch artifacts therefore need explicit revision names when reused.

Primary topology sources at the pinned main commit are the [project goal and
per-finger subsets](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org),
the [five-byte-per-hand packing and least-significant-bit
convention](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Software/README.org),
the [visualizer's slot and group
ordering](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Utilities/keyboard-visualization/src/main.rs),
and the [grouping
diagram](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Documentation/diagrams/keyboard-state-format-grouping.svg).
Together they support five three-position finger columns plus two thumb
positions per hand, or 17 slots per hand and 34 overall. They do not provide
metric coordinates, key dimensions, or a complete logical key manifest.

The following terms are strict:

- **Demonstrated** means a tracked artifact or result was directly inspected or
  recomputed from tracked input.
- **Exists** means a file, design, photo, model, or note is present, but its
  claimed behavior was not established.
- **Asserted** means author prose reports a result that this checkout cannot
  reproduce.
- **Missing** means the measurement needed for the claim is absent.
- **Contradicted** means tracked evidence conflicts with the claim or code
  contract.

## Evidence matrix

| Area | Status | What the repository establishes | What it does not establish | Confidence |
| --- | --- | --- | --- | --- |
| Product goal | Exists | Main describes a 34-key, keyboardless, six-axis IMU and ML [goal](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org). | A goal is not a working system or a complete logical key manifest. | High |
| Slot topology and packing | Demonstrated by source | The [software format](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Software/README.org), [visualizer](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Utilities/keyboard-visualization/src/main.rs), and [grouping diagram](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Documentation/diagrams/keyboard-state-format-grouping.svg) define 17 slots per hand and a packed group/bit order. | They do not establish metric geometry, dimensions, or a complete 34-position logical key map. | High |
| Physical prototypes | Exists | Photos, renders, parametric ring CAD, PCB sources, Gerbers, BOMs, and component models are tracked. | No fabrication log, electrical test report, wear test, fit distribution, drop test, or long-term reliability result. | High for presence, medium for function |
| Sensor topology | Demonstrated by source | Main firmware declares six sensors, reads five mux channels plus an onboard unit, and selects them sequentially ([`config.h`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Hardware/Firmware/mainboard/include/config.h); [`sensors.cpp`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Hardware/Firmware/mainboard/src/sensors.cpp)). | No validated two-hand topology, clock synchronization across two devices, or complete 34-key runtime. | High |
| Signal semantics | Demonstrated by source | Each reading contains linear acceleration and DMP yaw, pitch, and roll scaled into fields named `gx`, `gy`, and `gz` ([`sensors.cpp`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Hardware/Firmware/mainboard/src/sensors.cpp)). | The tracked data is not raw angular velocity despite the `g*` naming in several documents. | High |
| Six-sensor rate | Demonstrated, bounded | Eight useful [2023 timing logs](https://github.com/xjjak/LapCal/tree/8638759168d84d32fa50ae461e053055a01c3dff/Hardware/Firmware/scripting) show mean instrumented scan durations equivalent to 14.3-19.1 scans/s. | Stable end-to-end rate with transport, two hands, inference, and host input is unmeasured. | High |
| Main README rate | In tension; boundary not directly comparable | Main says the unified design reached ["about 10Hz at best"](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org), while the recomputed instrumented scan-boundary figures are 14.3-19.1 scans/s. | The repository does not show that the author estimate and timing logs measure the same end-to-end boundary or configuration. Neither supports a stable application delivery claim. | High for the two source values; medium for their relationship |
| One-sensor rate | Asserted estimate | The [controlled-taps note](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Software/data-collection.org) says 151,471 rows in about ten minutes, implying about 252 rows/s. | Raw files and timestamps are absent, duration is approximate, and this is not a six-sensor rate. | High about the boundary |
| Time synchronization | Demonstrated, bounded | The timestamp commit stores 1,000 wired samples with host-minus-MCU mean -14.998 ms and median -15 ms; another recorded run reports -12 ms ([`timestamp-analysis.org`](https://github.com/xjjak/LapCal/blob/3bb72ecaf0cfb4715ea82d00b4df920f19775cbd/Software/tools/timestamp-analysis/timestamp-analysis.org)). | It has no independent time reference, long-run drift, jitter distribution across sessions, BLE latency, two-device sync, or sensor-to-label accuracy. | High |
| Serial transport | Exists | Main can print one timestamped line containing six sensor readings ([`main.cpp`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Hardware/Firmware/mainboard/src/main.cpp); [`sensors.cpp`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Hardware/Firmware/mainboard/src/sensors.cpp)). | No tracked host parser completes live decoding and key injection. | High |
| BLE transport | Contradicted | Intended payload is 80 bytes, but packing iterates seven sensors into an 80-byte buffer and notification sends only 76 bytes ([`ble-utils.cpp`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Hardware/Firmware/mainboard/src/ble-utils.cpp)). | The tracked BLE path cannot be accepted as safe or protocol-conforming without repair and a runtime test. | High |
| Calibration | Exists, with defect | Per-sensor offset records exist for both hands. Main's non-right-hand middle and ring entries are identical, while the calibration result gives a distinct middle value ([`config.h`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Hardware/Firmware/mainboard/include/config.h); [`results-right.md`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Documentation/calibration/results-right.md)). | No repeated-session drift, temperature, don/doff, or calibration-time measurement. | High |
| Main 2025 data provenance | Asserted | The [collection notes](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Software/data-collection.org) name one author, one right middle finger, one physical sensor, dates, calibration, task, and approximate duration. | `_datasets` is ignored and absent, participant demographics are absent, and dataset hashes, device build, exact clock, and consent/license terms are absent. | High |
| Main label quality | Contradicted | The [notebook](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Software/data-collection.org) identifies seven erroneous taps, then the ML ref's [`preprocess_dataset.py`](https://github.com/xjjak/LapCal/blob/824dfd0710a93c4515c07399d40555dc31c59ffa/Software/preprocess_dataset.py) writes every replacement to row 0 instead of the loop row. | No corrected file is tracked, so downstream claims cannot be checked against fixed labels. | High |
| Historic state datasets | Demonstrated, flawed | The state branch tracks five raw collections and descriptions. Recomputed rates are 49.7-65.8 readings/s for four non-corrupt captures. Dataset 3 and click omit sensor 6. | Participant/session count beyond the apparent single author is unknown. Dataset 1 is explicitly corrupt; dataset 2 says typing errors; dataset 4 has severe clock-range mismatch. | High |
| Historic state split | Contradicted as evaluation | [`merge.py`](https://github.com/xjjak/LapCal/blob/178d1d09abd32191bf8c4ede54623facc1e385c8/Software/state-detection/merge.py) row-randomly splits overlapping 3-reading windows 70/18/12. Reproduction shows 99.11% of dataset-3 evaluation windows and 99.56% of dataset-4 evaluation windows share at least one raw reading with train. | It is not a session-held-out or user-held-out evaluation. | High |
| Historic state model | Missing | The [state preparation source](https://github.com/xjjak/LapCal/blob/178d1d09abd32191bf8c4ede54623facc1e385c8/Software/state-detection/prepare_dataset.py) defines 179 input features and 17 simultaneous key states. | The notebook is empty; no trained state model, inference metrics, or working runtime is tracked. | High |
| Historic click notebook | Exists, not cleanly reproducible | Saved outputs report a best displayed test F1 of 0.8007 for press-onset detection and several other model results. The 14,090-window shape matches tracked `dataset-3/data.txt`. | Its code names absent `data/data-jufo/data-middle.txt`; lineage is inferred, it row-randomly splits overlapping windows, and it detects click onset only. It does not identify a key. | High |
| 2025 naive click metrics | Exists, not reproducible | Main records F1 0.3845 (threshold), 0.5003 (filtered threshold), and 0.4792 (rolling window) in [`machine-learning.org`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Software/machine-learning.org). | The source dataset is absent and the same data is used to explore parameters and report scores. | High |
| CNN task | Demonstrated by source | The ML ref extracts 50x6 single-sensor windows, drops release labels, and trains a two-output press-versus-negative CNN ([`data_prepare.py`](https://github.com/xjjak/LapCal/blob/824dfd0710a93c4515c07399d40555dc31c59ffa/Software/data_prepare.py); [`data_split.py`](https://github.com/xjjak/LapCal/blob/824dfd0710a93c4515c07399d40555dc31c59ffa/Software/data_split.py); [`train.py`](https://github.com/xjjak/LapCal/blob/824dfd0710a93c4515c07399d40555dc31c59ffa/Software/train.py)). | It does not classify row/key, release, modifiers, chords, or text. | High |
| CNN split | Exists, incomplete provenance | [`data_split.py`](https://github.com/xjjak/LapCal/blob/824dfd0710a93c4515c07399d40555dc31c59ffa/Software/data_split.py) splits by recording name, which is stronger than row-random splitting. | Raw recordings are absent; author/user separation is absent; the same synthetic generator populates all splits; no split counts or manifest is committed. | High |
| CNN reproducibility | Contradicted | The [source](https://github.com/xjjak/LapCal/blob/824dfd0710a93c4515c07399d40555dc31c59ffa/Software/data_prepare.py) consumes an iterator while counting it, uses stale variable names for "stay still" acceleration, and generates unseeded random samples. No training log or data manifest is tracked. | Model weights cannot be regenerated or tied to a reported score. | High |
| TFLite artifacts | Demonstrated as artifacts | Files are 18,392 and 9,328 bytes. Both take float32 `[1,50,6,1]` and return float32 `[1,2]`; the smaller file has only one int8 weight tensor. | There is no full-integer MCU model, accuracy result, peak RAM result, inference latency, firmware integration, or device test. | High |
| Live integration | Contradicted | The state commit has feature and model adapters, but [`preview.py`](https://github.com/xjjak/LapCal/blob/178d1d09abd32191bf8c4ede54623facc1e385c8/Software/tools/live-preview/preview.py) is syntactically invalid, omits `Pipe`, and uses undefined `self.pipe`. | No OS HID, keyboard event injection, debounce/state machine, correction UI, or text demo exists. | High |
| Complete text entry | Missing | No artifact closes intent detection -> key identity -> text output. | No WPM, CER, KSPC, false activations/hour, rollover, modifier, shortcut, latency, fatigue, learning, or longitudinal result. | High |
| License | Demonstrated as stated restriction | The pinned root [declares CC BY-NC-SA 4.0](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org); the bundled Round-Anything CAD dependency is MIT. | CC is a poor fit for software/hardware source, no separate data/model license exists, and patent rights are not granted. This audit is not legal advice. | High |

The PCB files are concrete enough to estimate the physical baseline. Edge-cut
coordinates give a 12.45 x 19.7 mm sensor board and a 52 x 39 mm mainboard.
The sensor BOM has 10 placements including one MPU-6050 and one four-pin JST
connector. The mainboard BOM reports 57 components and includes an
ESP32-WROOM-32E, TCA9548A mux, onboard MPU6050, USB-C/CH340C, microSD, battery
power circuitry, and five external sensor connectors. It omits a complete
manufacturer-part-number and price manifest, battery, wiring, textile/ring
assembly, and manufacturing/test instructions. It therefore supports a size
and topology estimate, not a reproducible cost or build.

## Reproductions

All commands were read-only against `research/LapCal`. Temporary Python
dependencies were provided by `uv`; nothing was written into the nested clone.

### Six-sensor timing captures

Nine `.dat` serial-monitor captures contain 504,665 lines. The checked-in
[`timing_eval.py`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Hardware/Firmware/scripting/timing_eval.py)
points to a missing `../mainboard/timing_data.dat`, so the script is not
runnable as written. A standard-library parser recomputed each usable `All
readings` duration:

| Capture | Samples | Mean microseconds | Implied scans/s |
| --- | ---: | ---: | ---: |
| `2023-11-12-whole-reset` | 1,035 | 52,549.5 | 19.03 |
| `2023-11-12-after-readings` | 867 | 65,413.0 | 15.29 |
| `2023-11-12-rtl-reset` | 1,105 | 52,391.3 | 19.09 |
| `2023-11-12-all-readings` | 2,592 | 59,626.2 | 16.77 |
| `2023-11-17-fifo-core0` | 1,936 | 61,918.0 | 16.15 |
| `2023-11-17-additional` | 579 | 69,826.0 | 14.32 |
| `2023-11-17-fixed-wait` | 1,742 | 58,145.6 | 17.20 |
| `2023-11-17-removed-availability` | 1,579 | 57,204.1 | 17.48 |

Early files contain about five per-sensor reading records per aggregate scan;
later files contain six. They were not pooled. Some end-to-end loops add about
11-22 ms after the scan for formatting/output, so these are scan timings, not a
guaranteed delivered sample rate.

Reproduction shape:

```bash
python3 - <<'PY'
# Parse every timing *.dat line, group the numeric value following
# "All readings", and report mean microseconds and 1e6/mean.
PY
```

### Timestamp sample

The 1,000 stored pairs reproduce exactly:

```text
pairs: 1000
host_minus_mcu_ms: median -15.0, mean -14.998, min -15, max -13
host step values: 8, 10, or 12 ms
MCU step value: 10 ms
```

This is evidence of a stable short wired offset, not absolute clock accuracy.

### Historic dataset integrity and rates

Raw timestamp parsing produced:

| Dataset | Sensor rows | Span | Median interval | Mean rate | Missing sensor | Click events | Targets |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: |
| 1, explicitly corrupt | 969 | 0.045 s | 0.050 ms | nonsensical | none marked | 136 | 17 |
| 2 | 11,035 | 222.064 s | 20.033 ms | 49.69/s | none | 570 | 17 |
| 3 | 23,679 | 366.787 s | 15.247 ms | 64.56/s | sensor 6 always | 836 | 17 |
| 4 | 3,034 | 60.968 s | 20.003 ms | 49.75/s | none | 1,287 | 17 |
| click | 13,481 | 210.505 s | 15.205 ms | 64.04/s | sensor 6 always | 1,014 | 7 |

Dataset 1's clock scale is corrupt. Dataset 2 uses a trailing delimiter that
current `prepare_dataset.py` rejects as a seventh sensor. Dataset 4 is worse:
only 42 of 1,279 label events retained by `merge.py` fall inside the sensor
time span; most labels occur after the sensor stream ends. The single-key click
capture is overwhelmingly target `31` (952 of 1,014 press/release events), with
six sparse accidental targets.

Reconstructing current `merge.py` and `prepare_dataset.py` with NumPy 1.24.4
reproduced the committed full arrays exactly for datasets 3, 4, and click:

```text
dataset-3: X (23676,179), y (23676,17), positive cells 2405, exact equal
dataset-4: X (3031,179), y (3031,17), positive cells 149, exact equal
dataset-click: X (13478,179), y (13478,17), positive cells 1972, exact equal
```

Exact equality proves artifact lineage, not evaluation validity. Replaying the
split indices shows the overlap leakage described in the matrix.

### Historic click result boundary

The old click notebook saved row-random test scores for binary press onset. Its
highest displayed test result is:

```text
HistGradientBoosting: accuracy 0.95333, precision 0.76626,
recall 0.83846, balanced accuracy 0.90313, F1 0.80073
```

The named input file is absent. The tracked dataset whose derived size matches
the notebook produces 14,090 overlapping windows with 1,615 positives. Under
the notebook's split, 4,605 of 4,650 test windows (99.03%) share at least one
raw reading with a training window. A training rerun was attempted with the
pinned NumPy 1.24.4 and scikit-learn 1.3.0 environment, but the historic
gradient-boosting fit terminated on this ARM host before returning a score.
The saved notebook output is therefore an artifact, not a reproduced metric.

### Model binary inspection

The ML-ref TFLite files have these stable identifiers:

```text
model.tflite
  sha256 78c9240f690134dd422db52565c82fc61660459c4c9f5088f04aeb36e2fa88bc
  bytes 18392
model_quantized.tflite
  sha256 fe820fb3d3d34ae0b49864007d44750d0d1b000b7fe191e33a182049d6b242fe
  bytes 9328
```

FlatBuffer inspection found one subgraph and eight operators in each: two
Conv2D, two max-pool, reshape, two fully connected, and softmax. Both expose
float input and output. This supports only "small binary classifier artifact
exists".

### Firmware build boundary

No firmware build was forced. `platformio`/`pio` is not installed, each
relevant project requires a local `wifi.ini`, and only `wifi.ini.dist` is
tracked. Credentials were neither created nor synthesized. Static source
defects remain conclusive because their array bounds and byte counts are local
compile-independent facts; physical and radio behavior remains untested.

## Project use classification

| Artifact | Classification | Reason |
| --- | --- | --- |
| Product concept, photos, and project history | **Evidence only** | Useful prior-art and failure context; not a validated prototype contract. |
| Sensor-unit and mainboard KiCad/Gerber sources | **Evidence only; independently reimplement** | Concrete topology evidence, but project policy excludes artifact reuse. The old MPU6050/DMP design also lacks a test record and production package. |
| Parametric ring CAD | **Evidence only; independently reimplement** | Concrete geometry evidence, but project policy excludes LapCal artifact reuse. The Round-Anything dependency has separate MIT terms; that does not relicense LapCal's ring geometry. |
| Six-sensor ESP32 firmware | **Reject for reuse** | BLE memory/protocol defects, wrong calibration entry, credential-coupled build, low scan rate, and no end-to-end integration. Keep as evidence. |
| Timing captures and timestamp sample | **Evidence only** | Valuable quantitative bounds, but configurations differ and coverage is short. |
| 2023 state raw datasets | **Evidence only** | Useful schema and failure cases; corrupt/misaligned sessions, single apparent user, temporal leakage, and license ambiguity prevent benchmark use. |
| 2023 click notebook and joblibs | **Reject for evaluation** | Missing named input and 99% window overlap contamination. Algorithms may be read as exploratory examples only. |
| 2025 collection apparatus | **Experimental-method precedent only** | Direct keyboard labels for a single finger are a sound experimental idea. Build a project-authored capture path and prove timing accuracy with fresh data. |
| 2025 single-finger preprocessing | **Evidence only; independently reimplement** | Dataset-by-recording splitting is a useful lesson, but project policy excludes code reuse and the source has label-fix, iterator, stale-variable, and seeding defects. |
| TFLite binaries | **Reject for deployment** | No reproducible training record, accuracy, integer I/O, memory, latency, or MCU integration evidence. |
| Live-preview draft | **Reject** | It does not parse and does not inject operating-system input. |

## What is still missing

LapCal provides no valid measurement for any of the following:

- per-key precision/recall or confusion across the intended layout;
- end-to-end character error rate, corrected error rate, or words per minute;
- accidental activations during ordinary hand motion, measured per hour;
- press-to-host and press-to-visible-character latency percentiles;
- modifiers, chords, rollover, backspace, shortcuts, and held keys;
- separate users, hands, sessions, don/doff cycles, surfaces, postures, or days;
- calibration time, drift, and recovery after sensor or radio failure;
- battery life, thermal behavior, comfort, size coverage, durability, and cost;
- two-hand synchronization and packet loss;
- inference time and peak RAM on the target controller;
- privacy behavior and a supported desktop/mobile input-injection path.

## Decision for this project

Do not fork LapCal as the implementation base and do not use any of its scores
as a feasibility target. Carry forward three lessons instead:

1. Partitioning by finger can reduce label complexity, but it must still be
   evaluated as key identity and text, not merely tap onset.
2. Labels and sensor frames should share one monotonic acquisition timeline.
   Dataset files must be immutable, hashed, and split by whole session before
   windows or augmentation are generated.
3. A new prototype needs an early streaming-rate and synchronization gate.
   Do not spend time on model selection until the full sensor topology delivers
   timestamped frames at the target rate without loss.

These conclusions narrow the next architecture work: LapCal is a cautionary
baseline and source of test requirements, not proof that the chosen IMU
architecture can meet invisible-keyboard product goals.

LapCal's published 34-slot topology and packed ordering remain evidence facts.
Core-28, its six reserved positions, and the logical position-to-key map are
this project's design. Software consistency tests do not establish that the
physical positions are separable.
