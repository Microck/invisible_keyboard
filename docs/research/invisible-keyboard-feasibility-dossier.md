# Invisible keyboard feasibility dossier

Access date: 2026-08-09

Status: research snapshot. This document compares the broad option space. The
user later fixed the product boundary and prototype direction, so use the
[first passive-surface prototype roadmap](../implementation/first-prototype-roadmap.md)
for execution and the
[Wayfinder map](../wayfinder/invisible-keyboard-feasibility-map.md) for the
decision record. Use the
[source integrity audit](source-integrity-audit.md) for the implementation-driving
claim corrections made after the original research pass.

## Product decision after this research

The selected product must work on passive unmarked surfaces and use wearable
sensing only. The user chose LapCal's five finger IMUs plus one dorsal IMU per
hand as the hard topology reference, personal calibration, Core-28 exact input,
and no language model. LapCal establishes a 34-slot per-finger topology and its
packed group/bit ordering. It does not establish metric key geometry or this
project's logical Core-28 map. The implementation is independently written
under a no-artifact-reuse policy. That choice intentionally accepts more
key-identity risk than the optical recommendation below because environmental
sensing violates the chosen type-anywhere boundary.

The optical and instrumented-surface programs below remain useful alternatives
and laboratory truth methods. They are not the current product implementation.
The selected one-hand wearable build, wiring, protocol, electrical/contact
gates, and stop rules are planned in the execution roadmap linked above. No
wearable rig has been assembled and no physical gate has passed. Exact Core-28
calibration starts only after both hand rigs pass admission.

The planned Rev-A sensor path uses pulsed LSM6DS3TR-C data-ready edges, direct
motion-register reads, and a latest-sample cache feeding a 200 Hz aggregate
grid from 208 Hz sensors. Nominally, 8 reads per second per sensor are
intentionally decimated and remain visible through retained source-edge
timestamps. This expected decimation is not an acquisition overrun. Gate 0A
must use a captured cache-commit marker and a later aggregate-snapshot marker
to classify every observed edge, read, commit, collision, selection, and
deliberate decimation. Direct-register reads alone cannot prove that every
internal conversion was retained [K18].

The decisive Gate 0A artifact is one DSLogic Plus native little-endian `u16`
capture, not external evidence JSON, decoded NDJSON, or human aggregates. Ten
raw bits record INT1x6, D8, D9, SDA, and SCL. The full run requests
7,220,000,000 words at 2 MHz, produces 14,440,000,000 bytes, and scores at
least 3,601 seconds. The 20 MHz 10-second preflight and 2 MHz 10-minute soak are
non-decisive. Build, finalization, hashing, and scoring run in a Crabbox; only
the physical capture runs on the compatible analyzer host. See the capture
guide's [Canonical artifacts](../implementation/gate0a-capture-stack.md#canonical-artifacts),
[Box validation record](../implementation/gate0a-capture-stack.md#box-validation-record),
[Physical preflight sequence](../implementation/gate0a-capture-stack.md#physical-preflight-sequence),
[Physical-host capture](../implementation/gate0a-capture-stack.md#physical-host-capture),
[Crabbox finalization](../implementation/gate0a-capture-stack.md#crabbox-finalization),
and [GPL and firmware provenance](../implementation/gate0a-capture-stack.md#gpl-and-firmware-provenance).
The fixed firmware blob identities do not yet reproduce from the available
source snapshots, so redistribution remains a legal and supply-chain risk.

Guided setup uses the physical keyboard only as a non-admission precheck. `B`,
Space, Backspace, and both outer thumbs are passive-only. Browser
acknowledgment opens a 1.5-second response gate; a causal sensor observation
from both hands decides whether exactly one expected hand and finger made
contact. Passive centroids, final-threshold selection, and held-out validation
use disjoint evidence. Persisted success is
`CalibrationPhase::ValidationPassed`, serialized as `validation-passed`, with
`CalibrationSnapshot.heldout_validation_passed=true`. The resulting
`HeldOutValidation` authorizes the saved model and replay only; faults use
`HeldOutValidationError`. The artifact sets
`requires_wear_session_confirmation=true`. Live output requires a fresh
per-process `WearSessionConfirmation` because the system cannot detect
wearable removal, movement, or loosening.

The implemented live path treats output neutralization and durable evidence as
two milestones. The Linux Gate 0S safety evidence composes a native `uinput`
owner plus independent evdev consumer with a separate real-CLI recording-mode
coordinator test. It does not yet prove the full `run-calibrated uinput` path
inside one 250 ms end-to-end oracle. Exact evidence finalization has no
universal wall-clock deadline: a terminal run drains only chunks admitted
before its atomic cutoff, writes and syncs the footer-complete session, audits
it, and then writes a live-clock-report-v2 document bound to the exact session
SHA-256. A durable status appears only after the report file and parent
directory are synced on Unix.

## Verdict

An invisible keyboard is feasible, but that phrase currently names four
different products. They do not share the same evidence or risk.

| Product interpretation | Verdict | Confidence | Best current path |
| --- | --- | --- | --- |
| Blank but instrumented surface | Feasible now as exact spatial input | High | A blank multitouch or other spatially instrumented surface with a hidden registered layout |
| Passive unmarked surface with familiar spatial QWERTY | Plausible, not yet proved for the required hidden-layout condition | Medium | Optical contact and hand geometry, tested without language correction |
| Learned deterministic gesture alphabet | Feasible for mixed-character text, but it is a new input method rather than QWERTY muscle memory | Medium-high | A FineType-like wrist IMU plus IR camera system |
| Language-assisted phrase transcription | Feasible for constrained prose, but not an exact keyboard | High for prose, low for arbitrary keys | TapType- or TypeAnywhere-like finger evidence plus a declared language model |

The conditional recommendation is:

1. If a blank sensor surface counts as invisible, choose it. It directly
   observes contact location and is the lowest-risk route to exact input.
2. If the surface must be passive and unmarked while preserving familiar
   QWERTY, run the optical-plus-touch truth program in this dossier. Do not call
   the product feasible until the hidden-overlay, no-language condition passes.
3. Preserve a FineType-like learned gesture alphabet as the strongest fallback
   when exact mixed-character entry matters more than QWERTY transfer.
4. Treat wrist IMU alone as a phrase-transcription architecture. It cannot
   physically distinguish keys assigned to the same finger.

The first build recommended by this research snapshot was a shared
optical-plus-touch truth rig, not a polished keyboard. It was a planned rig
that could show whether contact time, finger identity, or surface position was
the missing fact. It was not built. The later wearable-only decision replaced
that execution path, while preserving optical or instrumented truth as an
optional laboratory method.

The research does **not** support all of these requirements at once:

- passive unmarked surface;
- no camera, wearable, or environmental sensor;
- familiar spatial QWERTY;
- exact arbitrary keys, symbols, modifiers, shortcuts, holds, and repeat;
- no language correction; and
- immediate cross-user operation with no calibration.

That combination removes every measured source of key position. It is a
physical observability problem, not a model-selection problem.

## Reader's fast path

This table is the shortest route through the handoff. The linked sections hold
the evidence, exact protocol, and limits behind each answer.

| Question | Short answer | Detail |
| --- | --- | --- |
| Is the idea possible? | Yes, in four bounded forms. Exact input on a blank instrumented surface has the strongest evidence. Passive hidden QWERTY is plausible but unproved. Learned gestures and language-assisted prose work only under narrower product contracts. | [Verdict](#verdict) and [product contracts](#four-separate-information-contracts) |
| Which option should be used? | Choose a blank instrumented surface if it is allowed. For strict passive-surface QWERTY, test accessible optical geometry. If a new alphabet is acceptable, preserve a FineType-like system. Use wrist IMU plus a declared language model only for phrase entry. | [Ranked options](#ranked-options) |
| What happens first? | Run P0: record 10 minutes of timestamped data from the exact candidate platform. Stop that platform if the needed data is inaccessible, capture terms are unacceptable, or timestamps cannot align to contact truth. If P0 passes, run the two-session P1 truth-rig screen. Stop passive QWERTY if the hidden-passive condition fails and the location oracle shows that position remains unobservable. | [The first decisive program](#the-first-decisive-program) |
| What is needed? | Use a supported optical sensor, independent reference camera, blank contact-truth surface, matched passive surface, fixed registration, and Linux host. Record immutable timestamped sessions, start with a causal nearest-key baseline, run one-variable oracle substitutions, and evaluate with session- and person-held-out gates. | [Hardware](#hardware-boundary), [software](#software-boundary), [data](#data-and-labeling-protocol), and [evaluation](#canonical-build-roadmap) |
| What can be spent or reused? | Inventory owned hardware first. Buy nothing for a product build before the product boundary and P0 are settled. Use the owned-hardware P0/P1 fixture envelope first, and do not fund custom PCBs, BLE polish, a large cohort, or scientific sensors yet. Project policy keeps LapCal and emg2qwerty artifacts outside the implementation and requires a separate rights review for any proposed reuse. | [Cost and stop-loss](#cost-envelopes-and-stop-loss) and [implementation reuse policy](#independent-implementation-and-no-artifact-reuse-policy) |
| What is proved versus uncertain? | Published work proves hidden-layout typing on an instrumented surface and narrower learned-gesture or phrase products under their stated tasks. It does not prove accessible, hidden, passive-surface exact QWERTY, full HID behavior, population transfer, long-term comfort, or commercial rights. | [Known gaps and claim-audit notes](#known-gaps-and-claim-audit-notes) |
| What did the human decide? | Wearable-only sensing on passive unmarked surfaces, an independently implemented per-finger/dorsal topology informed by LapCal, personal calibration, Core-28 exact input, and a planned one-hand wired falsification rig first. | [Wayfinder decisions](../wayfinder/invisible-keyboard-feasibility-map.md#decisions-so-far) and [execution roadmap](../implementation/first-prototype-roadmap.md) |
| Where is the evidence? | K1-K17 link the primary papers and official standards. The local artifact index links the detailed audits, comparisons, cost model, risk register, and Wayfinder map. | [Source registry](#source-registry) and [local artifact index](#local-artifact-index) |

## What product is being built?

### Four separate information contracts

#### 1. Language-assisted phrase transcription

The sensor observes tap timing and likely finger identity. A language model
chooses among the several QWERTY keys normally operated by that finger. TapType
and TypeAnywhere show that this can produce useful English phrase entry. It
does not establish password, identifier, code, symbol, shortcut, or exact key
input. The corrected result must never be reported as sensor key accuracy.

#### 2. Familiar spatial QWERTY

The person taps the remembered location of a normal QWERTY layout. The system
must observe contact and layout-relative position. Optical tracking, radar,
magnetic tracking, acoustic localization, or a spatially instrumented surface
can supply position in principle. A wrist IMU or sEMG channel does not directly
measure an absolute surface coordinate; whether its motion or muscle patterns
separate same-finger positions is an empirical, task-specific question.

This is the closest interpretation of "type without a keyboard" while keeping
touch-typing muscle memory. It is also the hardest useful interpretation.

#### 3. Learned deterministic gestures

Each symbol maps to a deliberate finger combination, finger posture, chord, or
sequence. Position on an imagined QWERTY layout is no longer required.
FineType is the strongest recent research precedent in this category. Tap and
Twiddler show that learned mappings can ship, but their physical controls and
learning contracts differ from an invisible keyboard.

#### 4. Blank instrumented surface

The surface has no visible legends or discrete keys, but it measures contact
coordinates. The 2018 "Typing on an Invisible Keyboard" study reached 31.3 to
37.9 WPM over three days on a large touch surface, compared with 41.6 WPM in
its visible condition [K1]. This is strong evidence for a hidden layout on an
instrumented surface. It is not evidence for an ordinary passive desk.

### Terms that must not drift

- **Invisible layout:** no key outlines or labels are shown in the typing
  region.
- **Passive surface:** the typing surface contains no spatial sensing array.
- **Instrumented surface:** the surface directly measures contact or position,
  even if it looks blank.
- **Exact input:** every advertised key usage and down/up state is scored
  without dictionary or language repair.
- **Phrase input:** the advertised output is corrected language, not physical
  key truth.
- **Learned input:** the person learns a new gesture alphabet. It is not
  familiar spatial QWERTY.

These terms define products, not implementation details. Changing one changes
which published result is relevant.

## Ranked options

### Ranking by the product each option can honestly deliver

| Rank | Architecture | Direct observation | Best supporting evidence | Main limitation | Confidence |
| ---: | --- | --- | --- | --- | --- |
| 1 | Blank instrumented touch surface | Contact time and 2D location | Hidden-layout touch study: 31.3 to 37.9 WPM over three days [K1] | It is still a physical sensor surface | High |
| 2 | Egocentric or overhead optical geometry on a passive surface | Hand pose and estimated surface contact location | StegoType and TouchInsight show live surface text entry [K2][K3] | Their headline live tasks showed a virtual keyboard; hidden-layout performance is not established |
| 3 | Learned wrist IMU plus IR camera gestures | Tap timing, finger combination, and posture | FineType supports letters and mixed-character gesture entry [K4] | New one-handed alphabet; no code-only result or complete HID semantics |
| 4 | Wrist IMU plus language decoder | Tap timing and likely finger | TapType: online N=10, 19.2 WPM and 0.6% CER after personal calibration [K5] | Same-finger key position is not directly measured; the cited result is prose unless the contract is narrowed |
| 5 | Optical plus IMU | Optical geometry plus high-rate impact evidence | Complementary physics and related hybrid precedents | No reviewed study proves that this exact fusion beats optical alone; synchronization and wear burden increase |
| 6 | Radar surface localization | Motion and registered surface position | mmKey reported greater than 95% single-key and greater than 90% multi-key recognition [K6] | Custom unavailable array, no comparable live WPM, correction-dependent word result |
| 7 | Magnetic multi-finger tracking | Calibrated 3D fingertip position | Finexus reported millimeter-scale lab tracking [K7] | Bulky short-range lab hardware and no continuous keyboardless text study |
| 8 | Acoustic, force, capacitive, or piezo support | Usually contact time or a coarse location | Several interaction primitives | No decisive passive-surface, ten-finger, continuous exact-QWERTY result found |
| 9 | Bilateral sEMG | Muscle activation and finger-intent evidence | emg2qwerty is a large continuous decoding benchmark [K8] | Physical-keyboard labels, expensive unavailable research hardware, poor generic transfer, biometric burden |

The ranking is conditional. Rank 1 wins only if the surface may contain a
sensor. Rank 2 is the recommended research lane for strict passive-surface
QWERTY. Rank 3 is the deterministic fallback if a new gesture alphabet is
acceptable. Rank 4 wins on cheap phrase entry, not on keyboard equivalence.

### Evidence that changes the original IMU-versus-optical comparison

The earlier [sensing comparison](sensing-architecture-comparison.md) made
dual-wrist IMU and egocentric optical tracking conditional co-leads. The full
Bikeshed review sharpened the product boundary:

- An IMU can provide tap and finger evidence, but it does not directly measure
  same-finger surface position. Classification from motion may still be
  possible; the selected wearable program must establish that separability on
  held-out passive-surface data rather than assume an absolute coordinate.
- Optical tracking is therefore the better strict-QWERTY research lane, but
  only after proving that the required camera or hand-tracking data is actually
  available to this project.
- A blank instrumented surface was underweighted in the earlier ranking. If it
  satisfies the product definition, it is simpler and lower risk than inferring
  the same coordinate from vision.
- FineType adds a stronger deterministic alternative than the earlier
  finger-sequence fallback because it includes a mixed-character task without
  pretending to recover a spatial keyboard.

### What the newer precedents do and do not prove

**FineType.** The CHI 2025 system used an ICM-42688-P IMU, an OV2710 wrist IR
camera, and ten IR LEDs [K4]. Its isolated-gesture evaluation used N=28 with
leave-one-person-out testing. Its live N=10 text study reported 35.1 WPM with
5.1% character error for letters and 15.0 WPM with 7.5% character error for
mixed input. The mixed task contained seven Enron strings and eight
CodeSearchNet snippets. This is meaningful mixed-character evidence. It is not
a code-only score, a spatial-QWERTY result, or proof of full key-down, key-up,
modifier, repeat, shortcut, and rollover semantics.

**TouchInsight.** The UIST 2024 system factorized touch event, finger identity,
and touch location from egocentric vision. It reported touch F1 0.99, finger F1
0.96, and 6.3 mm mean position error. Its online N=12 study reported 37.0 WPM
and 2.9% UER while using character and word language models [K3]. The live text
condition displayed a surface-aligned virtual QWERTY keyboard. Those results
support the sensing decomposition and a visible-layout product. They do not
prove hidden-layout typing.

**StegoType.** Its Quest 3/UmeTrack system reported online N=18 at 42.4 WPM and
7% UER without a language model [K2]. Its training set was large and its live
pipeline was real. The live condition also used a visible virtual keyboard.
The result supports optical feasibility but does not answer the hidden-overlay
question. No separate non-language metric is carried into this handoff because
its exact metric and protocol passage were not verified well enough during
synthesis.

Visible virtual keyboards help users maintain position and correct hand drift.
Removing the overlay is a material condition change, not cosmetic polish.

### Why the other options do not lead

- **Radar:** the strongest result depends on a custom Qualcomm aperture that is
  not available. A current TI board first needs an adjacent-key localization
  test. Buying raw-capture hardware before that result is waste.
- **Magnetic:** it is useful as a laboratory geometry upper bound, but current
  multi-finger implementations add emitters, coils, wiring, short range, and
  metal sensitivity before they add a text-entry result.
- **Acoustic:** it is a good contact and coarse-localization probe. Surface
  material, echoes, ambient sound, and multiple taps remain unclosed.
- **Force, strain, piezo, and capacitance:** a single wearable channel mostly
  supplies contact or deformation. A distributed surface array can supply key
  location, but then the product belongs to rank 1.
- **sEMG:** it is costly, placement-sensitive, biometric, and not shown here to
  transfer from physical-keyboard labels to keyless surface use.
- **Immediate sensor fusion:** fusion is not free accuracy. Add a channel only
  when an oracle substitution and ablation identify the fact it supplies.

## The first decisive program

### Program goal

Determine, before custom hardware or a large study, whether a locally
accessible optical stream can recover exact Core-28 spatial QWERTY input on an
unmarked passive surface, and whether a blank instrumented surface is an
acceptable successful alternative.

The program separates four questions:

1. Can this project access the needed sensor data on the intended platform?
2. Can the rig observe contact time, finger identity, and 2D location at all?
3. Does the passive hidden condition preserve enough geometry for causal exact
   key mapping?
4. If it fails, which missing fact would make it pass?

### Platform-access screen P0

Do this before selecting a headset, camera SDK, or paper implementation.

Required artifact:

- a 10-minute timestamped capture from the exact candidate platform;
- raw frames or documented hand landmarks with both hands, per-frame timing,
  confidence, and dropped-frame visibility;
- a supported local API and redistribution/deployment boundary;
- measured host receipt timing; and
- a minimal recording made without cloud processing or hidden vendor
  telemetry.

Pass only if the actual application can retrieve and retain the observations
needed by the experiment. A paper that used private UmeTrack access or an
internal camera path does not prove that a third-party Quest application can
access the same data. If the headset path is blocked, use an external camera
or a currently supported hand tracker. Do not build around an undocumented
preview or private API.

Stop if access depends on reverse engineering, unavailable private APIs,
unacceptable vendor capture, or timestamps that cannot be related to contact
truth. The fallback is an owned external camera or the blank instrumented
surface, not a compatibility shim.

### Observability screen P1

This is an adversarial engineering screen, not a usability study and not
population evidence.

Use one or two competent QWERTY typists. Each completes two sessions with a
complete camera/surface remount. Test this 2 x 2 matrix:

| Surface | Layout shown | Purpose |
| --- | --- | --- |
| Blank instrumented surface | Visible overlay | Validate registration, labels, and the easiest exact-input condition |
| Blank instrumented surface | Hidden overlay | Test whether the instrumented product can work while looking blank |
| Visually matched passive surface | Visible overlay | Measure the candidate optical geometry with visual anchoring |
| Actual passive unmarked surface | Hidden overlay | Test the strict product hypothesis |

For each person, session, and condition:

- collect at least 10 randomized activations for every Core-28 key;
- include same-finger adjacent-key pairs, ring/pinky occlusion, crossed or
  overlapping fingers, resting hands, deliberate hand shifts, and light taps;
- run at least five minutes of continuous no-correction copying; and
- arm at least five minutes of quiet, mouse, phone, object-handling, and normal
  desk movement negatives.

Core-28 means A-Z, Space, and Backspace. It is the complete advertised set for
this observability screen. Unsupported keys stay unsupported and cannot vanish
from the denominator. Core-28 is a provisional, screen-only event manifest.
The full product event manifest remains a human decision and must enumerate
every advertised HID usage plus down/up, hold, modifier, repeat, shortcut,
chord, and rollover behavior before Gate 5. Passing Core-28 does not qualify a
full keyboard.

The screen uses prompted random keys and mixed unfamiliar strings. It disables
all autocorrection, language models, word candidates, post-hoc smoothing, and
future context. An abstention is a missed key and counts as an error.

The provisional screen passes a condition only when:

- every Core-28 class is represented in both remounted sessions;
- all capture loss and clock residuals are visible;
- the causal nearest-key baseline has at most 20% top-1 key error overall;
- no class has point recall below 50%;
- the instrumented coordinate oracle maps prompted keys with at most 1% error;
  and
- no unintended modifier or destructive output exists, although P1 does not
  yet emit system input.

These are stop-loss values, not product thresholds. Gate 2 later requires the
stricter participant-level macro-F1 and event-error bounds in the
[feasibility contract](usable-text-entry-feasibility-contract.md).

Stop the passive spatial-QWERTY lane if the hidden passive condition fails and
the oracle analysis shows that location remains unobservable. Choose the blank
instrumented product or ask the human to accept a learned gesture alphabet.
Do not rescue the result with a language model.

### Hardware boundary

| Subsystem | First-program requirement | Why |
| --- | --- | --- |
| Candidate optical sensor | Owned camera, supported external hand tracker, or headset only after P0 | Avoid a purchase or architecture tied to inaccessible platform data |
| Independent reference | Owned high-frame-rate phone/camera, external trigger where available | Audit contact/finger labels and resolve candidate-tracker errors |
| Instrumented truth surface | Blank multitouch panel, tablet, or other surface that exports contact coordinates and timestamps | Supplies independent contact/location truth and tests the lowest-risk product |
| Passive surface | The actual intended desk/surface plus a visually matched plate where useful | Prevents instrumented-surface behavior from standing in for the strict product |
| Registration | Fixed external fiducials outside the typing area and a versioned plane/layout transform | Makes coordinate drift measurable without showing keys |
| Host | Linux workstation with local storage and no required network egress | Fastest reproducible capture, analysis, and later `uinput` path |
| Optional contact truth | Piezo/contact microphone or force switch used only as a timestamp oracle | Tests whether contact timing, rather than geometry, is the bottleneck |

Do not add wrist IMUs to the live decoder in this program. They may record as a
timestamped shadow channel if boards are already owned, but their data cannot
affect output until an oracle and ablation show that they improve contact time,
finger identity, or ordinary-movement rejection.

### Software boundary

Build the smallest replaceable research pipeline:

```text
optical observations ----\
touch/reference truth ----> immutable recorder -> causal evaluator
prompted key truth -------/                         |-> nearest-key baseline
                                                    |-> oracle substitutions
                                                    `-> scorecard and replay

deterministic event replay -> isolated key-state adapter -> Linux uinput
```

Required components:

1. **Recorder:** append-only session files with per-stream sequence numbers,
   device and host timestamps, hardware/firmware/software hashes, calibration,
   surface condition, overlay condition, and loss accounting.
2. **Registration:** a frozen surface plane and Core-28 layout transform. It
   produces a versioned artifact and never adjusts from test labels.
3. **Causal contact estimator:** it uses only observations available at the
   event time. It may abstain, but abstention remains an error.
4. **Nearest-key decoder:** map the estimated 2D contact to the closest frozen
   key region. Start here. Do not train a neural text decoder before this
   physical baseline is understood.
5. **Oracle evaluator:** replay the same session while substituting one ground-
   truth variable at a time.
6. **Target harness:** separately records deterministic key-down/up replay,
   application callbacks, rendered text, repeat, focus, and fault recovery.
7. **Scorecard:** preserves raw, pre-language, final, event, latency, negative
   exposure, and failure results with 95% intervals where the sample supports
   them.

### Three oracle substitutions

Run the real baseline, then exactly three one-variable substitutions:

| Oracle run | Substitute | Keep estimated | Question answered |
| --- | --- | --- | --- |
| O1 | Contact time/down-up | Finger and location | Is segmentation the limiting fact? |
| O2 | Acting finger | Contact time and location | Is self-occlusion or finger association the limiting fact? |
| O3 | 2D contact location | Contact time and finger | Is spatial geometry the limiting fact? |

Do not run an all-oracle result as a success claim. It only validates the task
labels and layout mapping.

Decision logic:

- If O1 passes and the real baseline fails, improve contact estimation or add
  an IMU/contact shadow channel, then require an ablation.
- If O2 passes, address finger tracking, view placement, or occlusion before
  changing the decoder.
- If O3 passes, geometry is the blocker. Improve optical calibration/view or
  stop the passive lane.
- If all three single-oracle runs fail, the event set, registration, or task
  contract is broken. Fix the truth rig before collecting more data.
- If the visible passive condition passes but hidden passive fails, the current
  result is a visible virtual keyboard, not an invisible one.
- If hidden instrumented passes while hidden passive fails, the blank sensor
  surface is the evidenced product and the passive product remains unproved.

### Data and labeling protocol

Freeze a manifest before each session containing:

- protocol, prompt corpus, Core-28 layout, and content hashes;
- random participant and session IDs;
- device serials, camera/hand-tracker version, surface version, host build, and
  capture settings;
- surface and visible/hidden condition;
- camera and surface remount, calibration input, calibration time, and whether
  calibration may enter a model;
- device timestamp, host receipt, contact truth, model decision, and later OS
  and application-observed time;
- raw observations, prompted intent, touch coordinates, finger labels, decoder
  hypotheses, abstentions, and final text; and
- consent, capture indicator, retention, encryption, deletion date, and network
  destinations.

Assign whole sessions and people to splits before windowing, normalization,
augmentation, or feature fitting. Never repair raw data in place. Hash raw
sessions first and record every derived transformation. The local
[LapCal audit](lapcal-evidence-audit.md) shows why row-random overlapping
windows, missing raw data, and ambiguous clocks can produce convincing but
invalid scores.

### What authorizes the next stage

The N=8/N=10 and longer studies begin only when:

- platform data access is supported and documented;
- the full truth rig has visible loss and usable synchronization;
- the hidden passive condition passes P1 without language correction, or the
  human explicitly selects the hidden instrumented or learned-gesture product;
- the baseline/oracle result identifies a plausible measured path, not a wish
  for a larger model; and
- deterministic output Gate 0S is independently safe.

No custom PCB, BLE product path, app-store work, participant cohort, or polished
UI is authorized before those facts exist.

## Canonical build roadmap

Gate 0A, Gate 0S, and Gate 0B are first-class gates in the one canonical
taxonomy. Gate 0A and Gate 0S are independent so acquisition quality cannot
hide output defects and a perfect replay adapter cannot hide broken sensing.
Gate 0B starts the cumulative physical-observability sequence.

| Gate | Required proof | Provisional go condition | Stop condition |
| --- | --- | --- | --- |
| 0A - acquisition integrity | One DSLogic Plus native raw capture with ten bits, 2 MHz, 7,220,000,000 `u16` words, 14,440,000,000 bytes, and at least 3,601 scored seconds, bound to one complete hand session | 100% raw-word sequence and fault accounting; every observed edge, read, collision, D9 eligibility fall, D8 snapshot, selection, and deliberate decimation classified exactly once; at least 99.9% valid receipt; spread at most 5 ms or a passed alignment ablation; and selected source reads complete before their snapshot boundary | Silent or multiply classified evidence, external/decoded evidence substituted for native raw, clock or payload-link ambiguity, wrong mux/register trace, non-latest selection, same-sensor INT1 before D9 eligibility, or an unexpected overrun schedule |
| 0S - deterministic output integrity | Linux `uinput` through an independent evdev consumer, 10,000 down/up records, native neutralization fault cases, plus a separate real-CLI coordinator and durable-evidence oracle | Native-observer p95 at most 100 ms, p99 at most 200 ms, observed all-up within 250 ms, zero stuck modifier/repeat; exact evidence becomes durable before terminal durable status, without claiming that finalization shares the 250 ms bound | Any stuck key, untraceable event, focus guessing, fallback paste, status before durability, or claim that the current compositional tests prove one full CLI-plus-`uinput` deadline |
| 0B - observable contact | Both hands; frozen detector; independent contact truth; at least N=5 held-out people, two real remounts each, 50 taps per finger/hand/person/mount, and equal quiet/scripted-motion exposure | Miss upper bound at most 5%; p95/p99 at most 100/200 ms; every finger recall bound at least 80%; zero negative latency, extra contacts, or negative false activations | Candidate sensor supplies its own truth, either hand or a real remount is absent, or the frozen roster changes after inspection |
| 1 - acting finger | Gate 0B detector frozen; participant-disjoint fitting/held-out evidence; both hands; at least N=5 held-out people, two remounts each, 50 taps per finger/hand/person/mount, and all required negative conditions | Participant-level macro-F1 lower bound at least 0.90; event-error upper bound at most 10%; every finger recall bound at least 0.80; zero extra outputs or negative false activations | Test people enter fitting, simultaneous activity is relabeled after observing output, or finger evidence fails |
| 2 - exact 34-position/Core-28 identity | Frozen representation; participant-disjoint splits; at least N=8 held-out people, two remounts each, and 50 events for all 34 physical positions in every person/mount slice; no language model | Participant-level macro-F1 lower bound at least 0.90 across Core-28; event-error upper bound at most 10%; every physical-position recall bound at least 0.80; zero enabled outputs on six reserved positions | Same-finger positions are unobservable, top-k replaces top-1, a held-out person enters fitting, or windows cross a split |
| 3 - live uncorrected text | At least N=10, two sessions, at least 1,000 characters/session, live causal decoder | Lower bound at least 15 WPM, UER upper bound at most 10%, completion at least 95%, Gate 0A and Gate 0S remain green | Offline-only result, future/phrase leakage, or final string not traceable to events |
| 4 - corrected phrase text | Locked language model and corpus; N=10, at least 20 phrases each from standard and mixed sets | Lower bound at least 20 WPM, final CER upper bound at most 3%, completion at least 95%, corrections at most 0.15/output character | Only corrected output retained or result presented as exact-input evidence |
| 5 - arbitrary keys, code, and shortcuts | Language disabled; randomized keys, generated secrets, mixed punctuation, code, every advertised usage, 100 modifier/hold/repeat/shortcut trials | Key-event error upper bound at most 5%, shortcut/chord lower bound at least 95%, at least 15 WPM, zero stuck/destructive state | Unsupported keys removed, text normalized, dictionary repair, or app result substitutes for event trace |
| 6 - session and remount transfer | N=10, three sessions at least 24 hours apart, final complete remount held out | WPM drop at most 20%, UER and miss rise at most 3 points, median calibration at most 2 minutes, p95 setup at most 5 minutes | Test session changes model, collection trigger, final threshold, normalization, or geometry before scoring |
| 7 - person transfer | Locked pipeline on at least 20 wholly unseen people | Base gate still passes and lower bound on individual pass fraction is at least 0.80 | Any test person enters tuning or poor users are removed |
| 8 - ordinary movement negatives | At least 30 armed participant-hours over N=10 with 200 interleaved intentional events | False activation upper bound at most 0.1/hour, miss upper bound at most 5%, no false modifier/destructive chord | Balanced negative windows, missing inactive denominator, or a hidden wake gesture |
| 9 - extended use and recovery | N=12, counterbalanced 90-minute candidate and physical-keyboard conditions | No adverse event, median discomfort rise at most 2/10, participant p90 at most 4/10, stable speed/error, completion at least 95%, runtime at least declared use | Pain/numbness, performance collapse, hidden operator recovery, stuck state, or data-sheet-only battery claim |

All numeric go lines are provisional engineering thresholds except where the
linked [feasibility contract](usable-text-entry-feasibility-contract.md) marks
a definition or protocol boundary as evidence-backed. The human must approve
final product thresholds.

### Canonical input integration order

Use one forward path:

```text
deterministic replay
    -> Linux uinput
    -> live local decoder
    -> USB HID appliance
    -> BLE HOGP appliance
```

1. **Deterministic replay:** prove the event vocabulary and key-state machine
   before sensing affects a computer.
2. **Linux `uinput`:** give the narrow device-creation process write access to
   `/dev/uinput` through the host's ownership, group mode, ACL, or udev policy;
   the kernel does not require root as such. Keep the recorder/decoder outside
   that permission boundary. Test application-observed down/up, modifiers,
   repeat, layout, focus, crash, and all-up recovery [K12].
3. **Live local decoder:** connect the frozen sensor event stream to the same
   adapter. A passed held-out report authorizes model/replay use only. Require a
   fresh per-process user attestation that neither wearable moved before live
   output. Any source, watchdog, identity, clock, output, or process fault
   terminally revokes that instance and cannot silently rearm. Keep raw,
   decoded, injected, and application-observed events separately traceable.
4. **USB HID:** after the live path is green, present a standard keyboard. Use
   the host layout and repeat behavior. Advertise only the tested rollover and
   usage set.
5. **BLE HOGP:** last, repeat pairing, bond deletion, MITM/authentication
   properties, radio loss, reconnect, sleep, power, latency, and all-up tests.

Do not begin with BLE, a Windows VHF driver, a macOS virtual-HID entitlement,
an iOS extension, or a browser-only keyboard. The detailed platform boundaries
are in [Operating-system input and compute paths](os-input-and-compute-paths.md).

## Cost envelopes and stop-loss

The cost report is an estimate-only screen. Prices drift and exclude tax,
customs, most shipping uncertainty, certification, and legal work. Labor is
shown at an illustrative USD 100/hour. No purchase is authorized by this
dossier.

| Lane | Prototype cash | Participant allowance | Engineering | What the spend can decide |
| --- | ---: | ---: | ---: | --- |
| Shared optical truth rig, compatible camera owned | USD 60 / 250 / 650 min/realistic/upper, plus an owned or separately quoted touch-truth surface | USD 500-2,000 for later gates | 120-300 h | Whether accessible optical data observes contact and geometry under visible/hidden and instrumented/passive conditions |
| All-new optical | USD 320 / 950 / 2,500 | USD 500-2,000 | 120-300 h | Same decision without owned camera/headset hardware |
| Host-decoded dual-wrist IMU shadow/fallback | USD 95 / 180 / 310 | Not priced until Gate 0B-2 paid hours are scheduled | 60-140 h | Whether wrist evidence supports contact/finger classification for phrase input or an earned hybrid |
| USB HID increment | USD 0-40 / 150 / 350 | USD 0-300 | 20-80 h | Whether frozen exact events survive cross-platform wired input behavior |
| BLE HOGP increment | USD 25 / 180 / 500 | USD 0-500 | 60-180 h | Pairing, fault, latency, power, and interoperability after USB passes |
| Radar | USD 240 board-level minimum / 1,450 realistic raw-capture lane / 3,500 upper | USD 250-1,000 | 160-450 h | Only whether a current EVM resolves adjacent registered keys |
| Magnetic | USD 300 / 1,500 / 7,000 | Up to USD 1,000 after bench success | 250-700 h | A scientific geometry upper bound, not text entry |
| sEMG | At least USD 5,768 listed floor, unavailable; realistic cost unknown | USD 500-3,000 | 300-900 h | Not a viable first procurement path |

The cost report records USD 230.70 as its 2026-08-09 planning conversion for
the historic EUR 200 ceiling. Its linked ECB daily feed does not reproduce the
historic rate, so that number is a dated sensitivity input, not an independently
verified exchange-rate fact. It can screen the cheapest IMU rig or an
owned-hardware optical fixture, but it does not cover participants or labor and
is not a project budget.

### Staged release of money

| Stage | Spend rule | Stop-loss |
| --- | --- | --- |
| Product boundary | USD 0 | Do not buy until "invisible," platform, layout, event set, privacy, calibration, and budget are chosen |
| P0 data access | Owned hardware and supported SDK only | Stop any platform whose needed observations are inaccessible |
| P1 observability | Owned/borrowed camera and touch truth first; at most the approved small fixture envelope | Stop strict passive QWERTY when no single oracle exposes a plausible fix |
| Gates 0A and 0S | One complete cheap acquisition topology plus the deterministic host-output harness | No custom PCB, battery, gateway, or larger cohort |
| Gate 0B through Gate 2 | Fund the second hand, independent truth, and frozen physical-study capture/analysis | No custom revision or later cohort before contact, finger, and exact-position evidence passes in order |
| Gates 3-5 | Host decoder, target harness, then USB HID | Stop on offline-only decoding, language leakage, hidden keys, or unsafe state |
| Gate 6 | Only now fund repeatable wearable or optical mechanicals | Stop if remount transfer fails without test-time adaptation |
| Gates 7-9 | Freeze modality and fund population, negatives, comfort, and power evidence | Stop generality, always-on, or safety claims when their bounds fail |
| Product | Separate authorization | Certification, counsel, production tooling, app distribution, and support are new budgets |

See [Cost viable prototype paths](cost-viable-prototype-paths.md) for item-level
prices, assumptions, and vendor links.

## Privacy, security, safety, licensing, and IP boundaries

### Required from the first capture

- Process locally. No raw sensor, image, inferred key, surrounding text,
  focused-app, clipboard, or correction context leaves the host by default.
- Show an unambiguous capture state. Optical recording stops on lock, logout,
  consent withdrawal, or bystander entry outside the approved protocol.
- Crop or mask outside the registered hand/surface volume before storage where
  possible. Do not record room audio for an optical experiment.
- Keep raw observations, truth labels, decoded events, application events,
  consent, and aggregate metrics in separate stores with separate access.
- Use random participant IDs, encrypted local storage, explicit retention, a
  derived-artifact map, and verifiable deletion.
- Never use typing rhythm, motion, hand geometry, or sEMG to identify a person.
- Use generated password-like strings only. Record expected/observed length,
  event counts, and local pass/fail, never plaintext, screenshots, or telemetry.
- Production logs are content-free. Counts, latency, loss, and version IDs are
  sufficient.

### Safe input boundary

- The model process has no shell, clipboard, window-control, or network
  privilege.
- A small adapter owns key state and accepts only the frozen allowlisted usage
  set through a local permissioned protocol.
- Neutral startup is mandatory. Duplicate down/up is a recorded defect.
- Planned stop and update send all-up. The native Linux owner and independent
  evdev consumer must observe neutral within the Gate 0S deadline. The real CLI
  coordinator remains a separate proof layer until one all-up oracle exercises
  the complete `run-calibrated uinput` path.
- Output follows OS focus. It never guesses or remembers a destination window.
- BLE pairing requires a physical action and short discoverable window. Record
  whether the achieved link is authenticated or merely encrypted.
- Firmware and model updates are signed, version-bound, rollback-controlled,
  power-loss safe, and neutral before reboot.

### Human and hardware safety

Start tethered. Do not add a body-worn battery before the sensing result earns
it. Use a stable smooth surface, neutral posture, breaks, skin inspection, and
stop immediately for pain, numbness, weakness, heat, abrasion, or loss of
control. Do not coach harder tapping to improve a classifier.

The 90-minute gate is a comfort screen, not long-term ergonomic proof. A
shipping wearable later needs qualified review for repetitive impact/posture,
skin-contact materials, battery and charging, thermal surface temperature,
RF exposure, ESD/EMC, mechanical reliability, and target-market product safety.

### Independent implementation and no-artifact-reuse policy

The canonical implementation must not copy, adapt, train from, or ship LapCal
or emg2qwerty code, data, weights, PCB, CAD, or models under this project's
conservative no-artifact-reuse policy. Their repository license statements and
available provenance do not establish all rights that a commercial product
would need [K8][K9]. This is a project policy and risk boundary, not a claim
that their Creative Commons terms prohibit every covered noncommercial use or
adaptation.

Project policy permits inspection and citation as research evidence. Any
particular reproduction or adaptation still depends on the applicable rights,
license terms, and exceptions. Write fresh requirements, source, PCB, CAD,
datasets, and models. Maintain a provenance ledger with source URL,
version/hash, owner, license, data/participant rights, patent grant if any, and
approved use for every dependency and artifact. The project has not documented
separated teams, access controls, or a communication record and therefore does
not claim a formal clean-room process.

Before reuse, perform an exact license and provenance review for:

- UmeTrack and any StegoType implementation, data, weights, or device access;
- TouchInsight source, data, weights, and platform APIs;
- FineType source, data, weights, CAD, and sensor design;
- TypeAnywhere's `DrustZ/TenFingerTyping` code, which is MIT-licensed and may be
  reused with its notice, and its separately hosted model weight, which must not
  be used without its own verified provenance and license; and
- any camera SDK, hand model, language corpus, pretrained model, footprint,
  firmware sample, or evaluation dataset.

A paper URL is not a software, model, data, hardware, or participant-rights
license.

### Legal and market boundary

The existing patent scan is not a freedom-to-operate opinion. After the sensing
flow, decoder, calibration, event set, markets, and form factor are frozen,
patent counsel must review live claims, families, territories, ownership, and
status. Privacy counsel must review the actual purpose and jurisdictions,
especially optical hand geometry and any identity use.

Before sale or distribution, separately scope FCC/RED/GB radio, RF exposure,
EMC, battery transport, product safety, Bluetooth qualification, USB
identifiers/marks, materials, accessibility claims, update/support life, and
privacy obligations. A working development module or HID descriptor is not a
certified product.

See [Privacy, safety, licensing, and IP constraints](privacy-safety-licensing-and-ip-constraints.md)
for the full threat model, jurisdiction examples, standards, patent leads, and
risk register.

## Open human decisions and recommended defaults

Nothing in this table is silently approved. The middle column is a recommended
starting default so the next human session can decide quickly.

| Human decision | Recommended default | Why |
| --- | --- | --- |
| What "invisible" excludes | No visible or marked key layout and no discrete keys; a flat surface is allowed | Preserves the interesting product claim without pretending that all hardware vanishes |
| Camera, headset, or wrist camera | Allow an external local camera in research; do not require a headset or wrist camera in the product until P0, privacy, and wearability pass | Keeps the first experiment accessible and prevents private platform APIs from owning the design |
| Passive versus instrumented surface | Test both in the paired rig; prefer blank instrumented if the human accepts it | It gives the fastest exact-input success while preserving evidence about the strict passive option |
| QWERTY versus learned alphabet | Test familiar spatial QWERTY first; preserve FineType-like gestures as fallback | Answers the stated keyboard ambition while keeping a credible deterministic alternative |
| Prose versus arbitrary keys | Make exact arbitrary key intent the primary product claim; add language-assisted prose only as a separately scored feature | Passwords, code, symbols, and shortcuts are what distinguish a keyboard from a phrase transcriber |
| First platform | Linux desktop | Best local capture, deterministic replay, and `uinput` path; no app-store or custom-driver gate |
| First event set | Core-28 for P1; freeze the full product HID usage/down-up/modifier/repeat/rollover set before Gate 5 | Keeps the physics screen bounded without mislabeling it full keyboard support |
| Calibration | Allow at most two minutes of personal calibration in early feasibility; always report zero-calibration separately | Gives the architecture a fair screen without hiding session-transfer cost |
| Metrics | Use the canonical gate contract and provisional thresholds as written; require raw and corrected panels | Stops a single accuracy or WPM headline from becoming the product claim |
| Abstention | Count as a miss/error | Prevents selective silence from inflating accuracy |
| Privacy | Offline/local, no network egress, raw capture only in explicit research sessions, content-free production logs | The device handles keystrokes and potentially cameras or biometric signals |
| Owned hardware | Inventory supported cameras, tablets/touch panels, Linux host, high-frame-rate phone, mounts, and test gear before procurement | Existing hardware changes the best cost path by hundreds of dollars |
| Cash stop-loss | Approve at most the owned-hardware P0/P1 fixture envelope first; release later budget only gate by gate | Avoids custom hardware and participant spend before observability exists |
| Product geography and claims | Private non-medical research until separately approved | Legal, radio, safety, privacy, and accessibility duties depend on these facts |

These decisions belong to the open Wayfinder tickets, not this research agent.

## Source registry

K1-K17 were accessed on 2026-08-09. The immutable LapCal links in K9 and the
primary ST datasheet in K18 were rechecked on 2026-08-29. Primary papers and
official standards establish the stated boundary. A metric from one task is
not assumed to transfer to another.

| ID | Source | Claim boundary |
| --- | --- | --- |
| K1 | [Typing on an Invisible Keyboard DOI](https://doi.org/10.1145/3173574.3174013) | Hidden layout on a large instrumented touch surface, three-day speed boundary |
| K2 | [StegoType DOI](https://doi.org/10.1145/3654777.3676343) and [author PDF](https://www.keithv.com/pub/stegotype/richardson_stegotype.pdf) | Quest 3/UmeTrack live optical surface typing, cohort, speed/error, training, latency, occlusion, visible virtual-layout condition |
| K3 | [TouchInsight DOI](https://doi.org/10.1145/3654777.3676330), [project](https://siplab.org/projects/TouchInsight), and [author PDF](https://static.siplab.org/papers/uist2024-touchinsight.pdf) | Touch/finger/location factorization, spatial and online text metrics, language models, visible virtual-layout condition |
| K4 | [FineType DOI](https://doi.org/10.1145/3706598.3714278) | Wrist IMU/IR hardware, gesture task, participant boundaries, letter and mixed-character live results |
| K5 | [TapType paper](https://arxiv.org/html/2410.06001) | Wrist sensing, personal calibration, finger evidence, language decoding, online N=10 result |
| K6 | [mmKey DOI](https://doi.org/10.1109/JIOT.2021.3084560) | Custom radar surface-key results, correction and hardware boundary |
| K7 | [Finexus DOI](https://doi.org/10.1145/2858036.2858125) and [author PDF](https://keyuc.com/research/Finexus/Finexus_CHI2016.pdf) | Five-fingertip magnetic tracking accuracy and lab topology, not text entry |
| K8 | [emg2qwerty paper](https://proceedings.neurips.cc/paper_files/paper/2024/file/a64d53074d011e49af1dfc72c332fe4b-Paper-Datasets_and_Benchmarks_Track.pdf) and [repository](https://github.com/facebookresearch/emg2qwerty) | Physical-keyboard-labelled sEMG benchmark, availability, and noncommercial reuse boundary |
| K9 | LapCal immutable [project goal and finger subsets](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org), [packed format](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Software/README.org), [visualizer](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Utilities/keyboard-visualization/src/main.rs), and [grouping diagram](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/Documentation/diagrams/keyboard-state-format-grouping.svg) | Exact 34-slot topology and packed ordering; no metric geometry or complete logical map |
| K10 | [USB HID 1.11](https://www.usb.org/sites/default/files/hid1_11.pdf) | Report state, boot format, rollover, modifiers, repeat, and LEDs |
| K11 | [Bluetooth HOGP 1.2](https://www.bluetooth.com/specifications/specs/hid-over-gatt-profile-hogp/) | BLE HID services, security, bonding, reconnect, and latency boundary |
| K12 | [Linux kernel `uinput`](https://docs.kernel.org/input/uinput.html) | Canonical userspace virtual-input bench route |
| K13 | [W3C UI Events](https://www.w3.org/TR/uievents/) | Key-down, key-up, repeat, physical code, logical key, and modifier semantics |
| K14 | [Soukoreff and MacKenzie text-entry metrics](https://www.yorku.ca/mack/chi03.html) | Error, correction, and KSPC definitions |
| K15 | [Wobbrock text-entry performance measures](https://faculty.washington.edu/wobbrock/pubs/text-07.pdf) | WPM timing and composition/transcription boundaries |
| K16 | [GDPR official text](https://eur-lex.europa.eu/eli/reg/2016/679/oj/eng) | Personal/biometric data, purpose, security, deletion, and DPIA boundary |
| K17 | [NISTIR 8259A](https://csrc.nist.gov/pubs/ir/8259/a/final) | Voluntary IoT security capability baseline |
| K18 | [ST LSM6DS3TR-C datasheet](https://www.st.com/resource/en/datasheet/lsm6ds3tr-c.pdf) | Direct output registers, block-data-update, pulsed-DRDY, and FIFO reset-mode semantics; the 208-to-200 decimation policy is a project design |

### Local artifact index

| Artifact | Use |
| --- | --- |
| [Domain context](../../CONTEXT.md) | Shared vocabulary and invariants |
| [Local evidence orientation](local-evidence-orientation.md) | Supplied ChatGPT session and research-corpus source map |
| [LapCal evidence audit](lapcal-evidence-audit.md) | Reproducibility, hardware, data, leakage, firmware, and reuse conclusions |
| [Source integrity audit](source-integrity-audit.md) | Oracle Deep Research reconciliation, primary-source corrections, and remaining physical limits |
| [Gate 0A DSLogic capture stack](../implementation/gate0a-capture-stack.md) | Canonical native raw capture, Box build/finalization, physical-host procedure, and GPL/firmware provenance |
| [Sensing architecture comparison](sensing-architecture-comparison.md) | Detailed modality matrix and primary evidence registry |
| [Prior-art landscape](prior-art-landscape.md) | Academic, commercial, repository, abandonment, and failure-mode landscape |
| [Usable text-entry feasibility contract](usable-text-entry-feasibility-contract.md) | Metrics, data rules, sample boundaries, canonical gates, and scorecard |
| [Operating-system input and compute paths](os-input-and-compute-paths.md) | Linux, Windows, macOS, Android, iOS, USB, BLE, browser, and compute boundaries |
| [Cost viable prototype paths](cost-viable-prototype-paths.md) | Itemized current price envelopes and gate-linked stop-loss |
| [Privacy, safety, licensing, and IP constraints](privacy-safety-licensing-and-ip-constraints.md) | Threat model, legal boundaries, safety, compliance, provenance, and risk register |
| [Wayfinder map](../wayfinder/invisible-keyboard-feasibility-map.md) | Canonical open decisions and human handoff |

The ignored `research/` corpus remains reference material. This dossier does
not modify, redistribute, or bless it for implementation reuse.

## Known gaps and claim-audit notes

- No prototype or participant study was run in this research pass. Every
  recommendation remains conditional on the canonical gates.
- No selected sensor signal can prove that a wearable stayed fixed after
  validation. Live operation intentionally depends on a fresh per-process user
  attestation and terminal fault revocation.
- FineType's DOI, hardware, study boundaries, and headline results were
  cross-checked during the Bikeshed review. During final synthesis, the indexed
  author-paper host timed out and ACM delivery returned HTTP 403. No public
  source code, data, model, CAD, or license was verified. Recheck the full
  primary paper and artifact provenance before implementation reuse.
- TouchInsight's public project and primary paper were available. The exact
  headset/platform data-access contract for an independent implementation is
  still unknown. That is why P0 is mandatory.
- Published StegoType and TouchInsight speed figures used visible virtual
  keyboards. They are not hidden-layout results. Earlier summaries that treated
  them as direct proof of invisible spatial QWERTY were too broad.
- FineType included limited CodeSearchNet snippets in its mixed task. Saying it
  had no code material is wrong. It still published no separate code-only score
  or complete key-event semantics.
- This dossier omits the debated StegoType non-language figure because its exact
  metric, denominator, and protocol passage were not verified during synthesis.
- TypeAnywhere's paper contains a cohort-count inconsistency: detailed method,
  IDs, and arithmetic support N=10 while one introductory sentence says five.
  The [prior-art report](prior-art-landscape.md) preserves that boundary.
- Kagi Assistant and Search were the primary research entrypoints. Primary
  papers and official pages were used for retained claims. Perplexity was
  blocked by an expired/invalid session, and gpt2agent Deep Research was blocked
  by a Turnstile challenge. Those lanes did not supply evidence.
- The 2020 optical paper could not be fetched directly in the architecture
  pass; its cohort and result boundary came from the official MIT record and
  the later StegoType paper.
- AirType/Noki and SelfieType are publicly silent, not proved cancelled. LapCal
  is not proved abandoned. Only Gest had a verified first-party shutdown and
  refund statement in this landscape.
- Prices, stock, currency, platform policies, SDK access, Bluetooth fees,
  standards, laws, and patent status drift. Recheck them immediately before a
  purchase, study, distribution decision, or legal review.
- The patent landscape is targeted discovery, not FTO. No claims were charted.
- No current inventory of the user's owned camera, tablet/touch panel, Linux
  host, phone, mounts, or test equipment exists. That inventory is the first
  cost input.

## Handoff status

The four Wayfinder decisions are resolved. The user selected wearable-only
sensing, the LapCal-style per-finger/dorsal topology, Core-28 exact taps, and an
independent implementation under a no-artifact-reuse policy. The optical P0/P1
program above remains a rejected option-space recommendation and a possible
laboratory-truth method, not the current product plan.

Use the [first prototype roadmap](../implementation/first-prototype-roadmap.md)
for the executable order: one-hand electrical and contact falsification, a
second admitted rig, two-hand guided calibration with locked passive-surface
validation, then live Linux output and broader human gates. All of those
physical stages remain planned and unpassed.
