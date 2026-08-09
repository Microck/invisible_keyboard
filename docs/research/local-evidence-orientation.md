# Local evidence orientation

## Purpose

This note records what the supplied local material actually establishes before
web research starts. It separates direct evidence, project intent, and open
questions so later work does not turn a promising prototype into proof it never
provided.

The source boundary for this pass is:

| Source | Role | Evidence boundary |
| --- | --- | --- |
| `research/n6epfy8ck7izu7f2.md` | Saved ChatGPT session | Historic user constraints and unverified product leads |
| `research/lapcal-issue-analysis.md` | Prior local analysis | Useful orientation, not an independent source |
| `research/LapCal` at main commit `8638759168d84d32fa50ae461e053055a01c3dff` | Abandoned reference implementation | Hardware, firmware, software, CAD, and authored project notes |
| Unmerged local `origin/*` refs in `research/LapCal` | Later experiments | Click-detection and data-collection work absent from main |

Generated KiCad output, vendored libraries, compiled model binaries, and media
were inspected only where they resolve architecture or evidence questions.
They are not treated as independent claims.

## Recovered intent

The current request is to make an invisible keyboard that permits typing
without an actual keyboard. The user wants multiple feasible paths where the
evidence supports them, a concrete recommendation, full build requirements,
and sources.

The saved ChatGPT session contributes three older constraints:

- the device was framed as robotics input, not medical assistance;
- the desired capability was information from fingers, not powered actuation;
- the target budget in that conversation was below EUR 200.

Those constraints are compatible with a low-cost sensing prototype, but the
current request does not restate all of them. The budget and acceptable form
factor therefore remain product decisions rather than silent requirements.

LapCal resolves the likely interaction family. Its main README states a goal of
typing on a 34-key layout without an actual keyboard, using six-axis inertial
sensors and machine learning (`research/LapCal/README.org:8`). Its presentation
describes a minimal glove that tracks fingers to simulate a keyboard or other
input method, with practical everyday use as the target. The shared product
boundary for research is therefore finger-driven keyless text entry. Speech,
gaze, and brain-computer interfaces are not equivalent alternatives.

## What LapCal contains

### Hardware shape

The project has two related architectures:

1. A wrist mainboard with an ESP32, an onboard MPU-6050, an I2C multiplexer,
   storage and power circuitry, plus five wired finger sensor boards.
2. A later modular concept with one small sensing module per finger and a wrist
   coordinator.

The tracked finger sensor PCB is about 12.45 by 19.7 mm and contains an
MPU-6050, passives, an LED, and a connector. The tracked mainboard is about 52
by 39 mm and has 57 BOM entries. Parametric ring CAD and photographs show a
progression from a fingerless glove to bare-hand rings connected to a wrist
board. The repository does not contain a priced, procurement-ready BOM, so it
does not prove the design fits the historic EUR 200 target.

### Firmware shape

The main firmware configures six IMUs, reads them through the multiplexer, and
streams measurements through serial or BLE. The output contains linear
acceleration plus yaw, pitch, and roll. Touch sensing and microSD recording are
present as unfinished or disabled paths.

The main README says the first unified design produced only about 10 Hz and did
not achieve proper synchronized data collection
(`research/LapCal/README.org:27-30`). The later one-sensor data-collection notes
estimate about 252 samples per second, but that is not evidence of the complete
six-sensor system at the same rate.

The current packet loop is unsafe as written. `pack_reading` iterates through
index 6 for a six-element sensor array, writes beyond the 80-byte packet, and
then sends a different byte count. This blocks any claim that the tracked main
firmware is a reliable end-to-end transport.

### Data and model shape

The main branch documents one small experiment for a right middle finger and
three positions corresponding to an upper, home, and lower key. It reports
roughly ten minutes of short taps and about 151,000 lines. The notes also report
unpredictable absolute-orientation drift and use exponential moving average
centering as a partial response (`research/LapCal/Software/data-collection.org`).

The preprocessing code contains a label-correction bug: a loop writes corrected
keyboard state to row 0 instead of the current row. Main contains no complete
training result and no committed end-to-end 34-key decoder.

Later unmerged refs materially change the evidence picture:

- `machine-learning@origin` at `824dfd07` adds model scripts, a long experiment
  notebook, and TensorFlow Lite binaries. Reported binary click-detection F1
  scores are about 0.38 to 0.50. The notebook does not document a completed
  34-key training result.
- `state-detection@origin` contains older offline click classifiers. Some
  tabled tests reach about 0.80 F1, while stream-like or imbalanced analyses are
  much weaker. These are single-purpose click tests, not text-entry proof.
- `64-sweep-data-collection-setup@origin` records a one-sensor data path for the
  right middle finger and three keys, using a physical Sweep keyboard for
  labels. It is not a two-hand glove evaluation.

The later model code still has correctness and integration gaps, including the
same row-0 label bug, inconsistent class assumptions, and a broken preview
path. Model binaries without reproducible datasets, training logs, and
end-to-end output cannot settle feasibility.

## What the local material proves

- A small ring or glove IMU package and wrist coordinator are mechanically
  plausible enough to prototype.
- Per-finger motion can carry some signal for contact or key-row inference in a
  constrained, personal experiment.
- Synchronization, drift, sampling rate, intent detection, and label quality
  are first-order problems rather than cleanup work.
- Decomposing a keyboard by finger or key zone can reduce the classification
  problem, but LapCal did not validate a complete 34-key system.
- The repository is a valuable failure archive and mechanical starting point,
  not a working invisible keyboard.

## What remains unproven

- continuous text entry rather than isolated taps;
- all fingers, both hands, modifiers, press and release semantics, and rollover;
- cross-session and cross-user performance;
- false activations during ordinary non-typing hand movement;
- usable words per minute, correction load, latency, fatigue, and learning time;
- a stable six-sensor sample rate and transport;
- a complete operating-system input path;
- current prototype cost, battery life, and everyday wearability;
- whether inertial sensing is better than contact, strain, acoustic, optical,
  radar, electromyography, or a hybrid;
- whether the noncommercial share-alike license permits the intended reuse;
- patent scope and freedom to operate.

## Consequence for planning

The next research pass must compare modalities before selecting hardware. It
must evaluate full text-entry behavior rather than celebrate isolated
classifier accuracy. The first human decision must also define what
"invisible" excludes, where typing happens, and whether a personal calibrated
prototype is an acceptable first product. Those choices are open tickets
because the user is away and the saved session cannot answer for them.

