# Guided personal calibration experience

## Decision

The first calibration flow uses a regular physical keyboard as a temporary
label source, then performs a shorter transfer and validation block on the
passive unmarked surface. The wearable remains the only product sensor. The
regular keyboard is setup equipment, not an input dependency after setup.

The setup app must not ask the user to measure keyboard angle, switch travel,
key spacing, sensor angles, or hand geometry. Accelerometer and gyroscope data
supports a gravity-referenced neutral-orientation proxy and relative motion; it
does not directly contain absolute hand pose or key position. A measurement
that cannot be inferred must have a demonstrated effect before it becomes a
user question.

The app must not claim "100% calibrated." It reports three separate facts:

1. sensor health and timing are acceptable;
2. every required position has enough usable examples; and
3. a locked decoder passed a passive-surface validation block.

Only the third fact produces `CalibrationPhase::ValidationPassed`, serialized
as `validation-passed`, and a `CalibrationSnapshot` whose
`heldout_validation_passed` field is `true`. Failure is
`CalibrationPhase::CalibrationFailed`, serialized as `calibration-failed`.
The passed result authorizes the saved model and replay. It does not authorize
live output or prove that either wearable stayed in place. Calibration can
still fail after enough samples. More typing is not a valid response to an
unobservable key class, a bad sensor stream, bad timing, or a domain mismatch.

Evidence labels in this document are:

- **Source-verified:** supported by an inspected external or local source.
- **Statically verified:** established by code, schema, or arithmetic without
  physical hardware.
- **Replay-tested:** established by deterministic recorded or generated input.
- **Simulated:** established only inside the explicit signal model.
- **Hardware-dependent:** impossible to validate without the wearable.

Rules marked provisional are starting choices, not an additional evidence
class.

## Why keyboard-only calibration is insufficient

A physical keyboard is an excellent label source. Its key events identify the
pressed key without asking the user to annotate every motion sample. The
emg2qwerty benchmark used two wrist sEMG devices while participants typed
prompts on an Apple Magic Keyboard with a US-English layout, recording
key-down and key-up times [C1]. That supports the keyboard-label collection
pattern for its muscle-signal setting. It does not establish transfer to this
IMU placement, mounting scheme, or a keyless surface. The paper itself warns
about the physical-keyboard versus keyless force-domain mismatch.

The press mechanics differ:

- a switch actuates after some key travel;
- a keycap constrains the contact location and direction;
- bottom-out, spring, and case response change the acceleration impulse; and
- a passive desk has no key boundary, travel, or force curve.

Therefore a decoder that validates only while the user types on a keyboard has
proved keyboard-labelled wearable recognition, not invisible typing. The
passive-surface stage is the smallest honest transfer check.

The two stages serve different jobs:

| Stage | What it supplies | What it cannot prove |
| --- | --- | --- |
| Physical keyboard warm start | Many cheap key labels, natural transitions, personal finger use, key-down and key-up timing | Passive-surface contact, imagined key spacing, or keyless accuracy |
| Passive-surface transfer | Contact dynamics and personal invisible-layout motion for the validated wear configuration | Future remounts, new surfaces, long-term comfort, or another user |

## User promise

The intended first-run experience is:

```text
Connect wearables
      |
      v
Identify the label keyboard and confirm its physical layout
      |
      v
5-second sensor check
      |
      v
Rest hands for neutral pose
      |
      v
Type guided text on a normal keyboard
      |
      v
Move the keyboard away and tap short guided targets on the desk
      |
      v
Run a locked held-out target and ordinary-motion check
      |
      +---- pass ----> Validation passed and files saved
      |
      +---- fail ----> Specific repair or unsupported-use message

Start one live process
      |
      v
Explicitly attest that neither wearable moved
      |
      v
User-attested wear session active
```

**Provisional target:** median setup at most 2 minutes and p95 at most 5
minutes after the hardware and prompt set have been optimized. These are the
Gate 6 targets from the feasibility contract, not achieved measurements.

The frozen first engineering plan cannot honestly promise that target. It has
102 passive centroid prompts, 10 passive threshold prompts, 102 held-out
position prompts, and 132 seconds of scripted ordinary motion before gaps and
keyboard warm start. Its hard cap is 1,200 seconds. The app must record actual
time in each state so a later version can reduce the measured expensive part
without weakening or reusing evidence.

## What the user provides

For Core-28 on the 34-position physical grid, the user provides only:

- confirmation that the displayed US QWERTY layout matches the physical
  keyboard;
- the physical keyboard to use for warm-start labels; and
- normal participation in the shown exercises.

US QWERTY is the only v1 logical layout. A different layout must stop with a
clear unsupported-layout message. It must not silently remap browser text.
The letter rows look like QWERTY, but finger ownership is this project's
contract: `B` belongs to the left index finger, Backspace to the left functional
thumb, and Space to the right functional thumb. The two outer-thumb positions
remain reserved. The confirmation screen must show those assignments instead
of describing the layout as ordinary QWERTY without qualification.

The app may collect these facts as optional study metadata, never as required
calibration inputs:

- laptop, low-profile, membrane, or mechanical keyboard category;
- flat, tilted, split, or tented posture category;
- desk material category; and
- touch-typing experience.

The app must not request:

- switch travel in millimeters;
- keyboard or wrist angle in degrees;
- manual sensor orientation values;
- hand or finger measurements;
- free-form personal text; or
- passwords, account names, source code, or private messages.

If later evidence shows that one of these physical factors causes a repeatable
failure, the preferred repair is sensor-based inference or a mechanical mount
constraint. A new question is the last choice.

## Canonical local data path

The calibration app is a local host service plus a browser front-end served
from `127.0.0.1`. It has no cloud path.

Each accepted HTTP connection has a cumulative 100 ms request-read budget
covering both headers and body, followed by a separate cumulative 100 ms
response-write budget covering headers and body. Partial progress never resets
either deadline. These budgets use real monotonic time, independently of the
capture clock used to timestamp browser receipts at connection acceptance.
Thus a peer can consume at most 200 ms of socket-wait budget in one service poll,
plus operating-system scheduling and local request processing. A timed-out
request receives HTTP 408 when the peer can read it; a response timeout closes
only that connection. Malformed requests and disconnected peers also remain
connection-local. The next poll can resume sensor draining or handle Abort
instead of waiting indefinitely for an unauthenticated trickling client.

Ordinary keyboard dispatch processes at most 256 queued events per loop turn,
matching the bounded sensor-drain batch. A continuously replenished keyboard
queue must not prevent browser Abort/heartbeat handling, sensor processing, or
the 1,200-second setup-cap check. Remaining events stay queued in FIFO order.
Worker transitions hold the keyboard-delivery gate while draining the whole
256-event-capacity queue, so every pre-transition event belongs to the old
worker and later delivery belongs to the new worker. If that gate is contended,
the service drains only one finite batch before returning to the main loop and
retrying the transition.

```text
wearable USB streams ----------------------\
                                            -> local Rust calibration service
selected physical keyboard EV_KEY stream --/           |
                                                        v
                                               browser progress UI
                                                        |
                                                        v
                                           immutable labeled sessions
                                                        |
                                                        v
                                           calibration artifact + audit
```

The local service, not browser `keydown`, is the canonical physical-keyboard
label source on Linux. It reads the selected keyboard's kernel `EV_KEY` events
and timestamps. The browser only renders prompts and progress. This avoids
browser-focus loss, layout-dependent character conversion, and JavaScript
event timing becoming hidden label noise.

W3C UI Events distinguishes a physical `code`, logical `key`, key-down,
key-up, modifiers, and repeat [C2]. The label recorder must preserve equivalent
physical and logical fields rather than reducing every event to a character.

During calibration:

- decoded invisible-keyboard output is disabled;
- only the chosen physical keyboard can supply labels;
- key-down, key-up, repeat, and modifier state are recorded;
- overlapping presses are retained as raw evidence but excluded from the
  Core-28 single-tap fit;
- sensor candidates and invalid records remain in the session; and
- the UI reconstructs prompt progress from the service's accepted label
  events.

The keyboard device is selected by an explicit identify step such as "press
left Shift on the keyboard you will use." The service records its stable
kernel identity, vendor/product values when present, and physical path. It does
not grab every keyboard on the host. Identification does not assert a logical
layout. The next screen displays the exact supported physical letter rows and
requires an explicit US-QWERTY yes/no decision. A no decision enters a named
unsupported-layout terminal state. A yes decision records the frozen copy
hash, prompt ID, dedicated instruction revision, and service accept time before
neutral capture can start.

The repository implements the selected `EV_KEY` adapter, dual-controller
transport, authoritative concurrent capture worker, neutral report, journal
matcher, evidence fitter, held-out evaluator, loopback service, and browser UI.
`calibrate-hardware` joins those pieces into one executable Linux workflow.
That path is software-tested, but no physical rig has run it. A successful
synthetic or replay run remains software evidence, not a live-device claim.

## Time alignment

The label and sensor clocks must be aligned before any training window is cut.

Each wearable provides:

- monotonic controller time;
- a per-hand sequence number;
- host receipt time; and
- one sample-time offset for each of the six sensors.

The planned sensor path produces pulsed 208 Hz data-ready observations and
selects the latest complete sample for each 200 Hz aggregate. Nominally, 8
reads per second per sensor are intentionally decimated. Every selected sample
keeps its source-edge timestamp. This declared rate conversion is admissible
evidence, not an acquisition fault; unexpected pending-read, read, freshness,
deadline, or transport failures remain faults.

Each physical key event provides the kernel event timestamp and host receipt
time. The service fits controller-to-host clock mappings from the raw session
and stores residuals. It never edits device timestamps in place.

The v1 fitter uses an affine mapping from controller microseconds to host
monotonic nanoseconds. A fit requires at least eight strictly monotonic pairs
over at least 100 ms. The initial fail-closed gates are absolute drift at most
5,000 ppm and p95 absolute residual at most 2 ms. The report retains sample
count, fit interval, offset, slope, drift, p50/p95/p99/maximum residual, and
the controller boot identity. Reset, stall, regression, or a new boot identity
invalidates the fit; the service does not splice epochs. These thresholds are
provisional and must be tightened from physical USB timing evidence.

Offline calibration and replay apply that strict fit to every decoded receipt
pair in the complete session. They do not discard a delayed receipt. Live
operation has a separate causal policy: it fits the maximal newest contiguous
250 ms suffix by controller time, still using every pair in that suffix and
the same drift and raw-residual gates. Startup stays disarmed for at most 500
ms while a delayed receipt prefix ages out. After a fit passes, a quality-only
candidate failure can use the unchanged last-good fit only within its existing
250 ms extrapolation limit. Every mapped frame must remain within 20 ms of its
current receipt, and a structural fault never enters holdover.

The live clock report names those recent fits as operational. Its v2 binding
includes the acquisition-manifest hash, session ID, and SHA-256 of the exact
strictly audited, footer-complete session bytes. A report from another valid
session with the same header is rejected. The report embeds a full-session
epoch only when the separate complete-session fit passes. A run can therefore
operate safely but remain unqualified as offline clock evidence; the raw
session and the exact qualification error are retained. Receipt timing cannot
reveal one coherent constant transport delay, so this policy proves only its
declared timing gates, not absolute physical event time.

For calibration only, an event window may include samples after physical
key-down because model fitting is offline. The live decoder remains causal.
The source window, live window, and timing reference must be named separately
in the artifact so future samples cannot leak into a claimed live latency.

Initial alignment rule:

1. Reject a sensor window with an invalid channel, clock-unsynced flag,
   acquisition overrun, or more than 5 ms inter-sensor skew. Do not reject the
   declared 208-to-200 latest-sample decimation; use the retained source-edge
   timestamps when applying the skew rule.
2. Match one physical key-down to at most one sensor impulse.
3. Retain unmatched key events as misses and unmatched impulses as negatives.
4. Estimate the event offset from accepted matches inside a frozen broad search
   range.
5. Freeze the offset and matching window before scoring held-out examples.

The exact search range is provisional until the one-hand rig produces data.
The decisive event metric still uses the feasibility contract's frozen
`[-50 ms, +250 ms]` intent matching window.

## Neutral pose instead of angle entry

At the start of every mount, the UI asks the user to rest both hands in a
comfortable home pose for three seconds.

The physical capture worker must derive:

- per-sensor gravity vector statistics;
- gyro bias statistics;
- finger-to-dorsal relative orientation statistics;
- sample jitter and per-sensor timing skew; and
- noise and shared-motion energy.

This neutral block can reject evidence inconsistent with the declared axis map
or stable capture, such as a missing, saturated, or visibly unstable channel.
It cannot prove that a wearable did not move, loosen, or get removed after the
block. It supplies a gravity-referenced relative-orientation and motion
baseline, not absolute hand pose, key coordinates, or mount-continuity sensing.
The raw baseline remains part of the immutable session. Derived normalization
is fitted only from training blocks.

The capture worker derives and persists gravity, gyro-bias, relative
orientation, jitter, skew, noise, and shared-motion statistics from a
source-bound session prefix. The service emits `NeutralAccepted` only when the
three-second block is stable. Synthetic tests exercise the calculation; only a
physical run can establish useful limits for the selected mounts.

If the neutral block is unstable, the UI identifies the sensor and asks the
user to reseat that mount. It does not let the user type an angle to override
bad evidence.

## Guided keyboard warm start

### Prompt types

One phrase corpus cannot cover Core-28 safely. The version 1 front-end combines
three frozen prompt kinds:

1. **Natural phrases:** lower-case words and spaces for comfortable rhythm.
2. **Coverage phrases:** generated or curated text that raises rare letter
   counts without long nonsense strings.
3. **Reserved-key prompts:** Semicolon, Comma, Period, and Slash are labeled even
   though Core-28 must abstain on those positions during normal use.

The prompt corpus is versioned and hashed. The app never asks the user to type
free-form text, because calibration logs inferred text by design.

Version 1 owns a project-authored natural phrase set, deterministic rare-key
coverage prompts, and one reserved-position prompt. It has no correction drill,
Backspace drill, or separate practice mode. The screen shows only the selected
current prompt; it does not list unused corpus targets. Prompt selection is a
pure function of the frozen corpus version, coverage shortfalls, and attempt
number. The corpus SHA-256 is present in both the acquisition manifest and the
calibration artifact.

`B`, Space, Backspace, left outer thumb, and right outer thumb are
passive-only. `B` cannot get an unambiguous hand label from the supported
keyboard arrangement, and the two functional thumb keys do not reproduce the
required passive-surface thumb contact. The outer-thumb positions have no
standard keyboard labels. Physical-keyboard events for these positions can
remain in the raw journal but cannot advance coverage, fit a centroid, or
choose a threshold.

### Adaptive selection

After each accepted event, the coordinator updates per-position coverage. For
each keyboard-eligible position it computes the exact deterministic score:

```text
score(position) = count_shortfall(position)
                + separated_block_shortfall(position)
```

The next phrase maximizes the sum of those scores for its expected positions.
Equal-score prompts rotate deterministically by attempt number. The five
passive-only positions never contribute a keyboard score. This is adaptive
prompt selection, not a language model in the decoder.

An event counts toward calibration only when:

- the physical key is in the frozen 34-position manifest;
- the press is not an OS repeat;
- the complete down/up press stays free of overlapping keys and modifiers;
- all six sensors for the acting hand are valid;
- no unexpected acquisition-overrun, clock-unsynced, or invalid-sensor flag is
  set; declared latest-sample decimation is not such a flag;
- timing and clock-fit limits pass; and
- the impulse can be matched one-to-one with the physical event.

Rejected events remain in the raw session and get a reason. They do not advance
coverage.
An unreleased press cannot advance coverage. Accepted labels retain the
original key-down time and capture context, not the later release time.

### Initial stopping rule

**Provisional:** the warm-start block is complete when all of these hold:

- every one of the 29 keyboard-addressable positions has at least eight
  accepted examples;
- each position appears in at least two separated time blocks;
- every expected finger sensor supplies usable examples;
- the clock and stream health checks remain green; and
- the strongest finger sensor agrees with each keyboard label.

This is a non-admission signal precheck, not key-identity accuracy. It cannot
fit passive centroids, choose the final decoder threshold, or unlock output.
Exact key identity is scored only after the passive model is locked.

These are setup heuristics, not product evidence. The full Gate 2 decision uses
session-held-out participants, mounts, confidence bounds, and at least 50
events per class per mount.

The current physical runner stops if the disjoint signal check fails. It does
not relabel the same attempt, learn from the check, or hide the failure behind
more text. A later retry policy must use a new evidence block and remain under
the same per-position and 1,200-second caps.

## Passive-surface transfer

The UI then asks the user to move the keyboard away without removing the
wearables. The target surface has no sensors, electronics, projection, or
permanent marks.

The front-end shows one large target plus a highlighted, screen-only
34-position physical grid on the monitor. It asks the user to:

1. rest the fingertips in an imagined home row;
2. tap one highlighted position at a time on the desk;
3. return to the comfortable home pose between prompts; and
4. stop immediately for discomfort.

**Provisional initial dose:** three accepted prompted taps for every one of the
34 physical positions, including the six reserved positions. This is 102
centroid-fitting taps. A separate ten-target passive block plus paired quiet
windows selects the final decoder threshold. The centroid, threshold, and
held-out windows are disjoint. The current plans are fixed and non-adaptive so
their dose cannot depend on model output.

Each target has two rendered states: a preparation instruction and a distinct
`Tap ... now.` cue. The local service does not create the label until the
visible browser acknowledges, after two animation frames, that the exact cue
revision reached the DOM. The acknowledgment opens a response gate from 0 to
1.5 seconds after its accepted host time. It does not assert that a contact
occurred. The capture retains a 1.8-second target segment. All later schedule
times shift by measured rendering delay, and both fitting and held-out attempts
persist their instruction receipts. Losing focus, the browser lease, or the
receipt stops the hardware attempt instead of inventing a server-timer label.

A causal sensor trigger, frozen before passive fitting, searches that response
gate. It inspects both hands and accepts only one contact whose hand and finger
match the prompted position on the physical grid. It records these distinct
rejected outcomes without advancing coverage:

- no contact in the response gate;
- contact from the wrong hand;
- contact from the wrong finger; or
- more than one causal contact.

The prompt supplies intended position only after this contact check. The cue
acknowledgment cannot become a synthetic tap, and position truth cannot choose
which sensor peak wins. A synchronized study camera may audit the contact and
finger result during engineering sessions, but it never enters calibration
features or product decoding.

The service can prove which plan revision it published, which acknowledgment it
accepted, and which sensor records it retained. That software receipt is not a
hardware attestation that the cue was visible, that the wearable was mounted as
declared, or that a person made the prompted contact. Physical studies still
need an honest capture operator and independent observation when authenticity
matters.

Model fitting and final held-out evaluation run on bounded background workers.
The foreground loop keeps serving browser heartbeats and sends both controller
streams through a discard-only integrity monitor. That interval is never
finalized or reused as calibration evidence. A source fault or lost browser
lease still ends the attempt while computation is in progress.

The transfer fit produces a new artifact and records the source domain of every
example. Version 1 fits position centroids and variance only from the 102
accepted passive-surface centroid windows. It selects the final decoder
threshold only from the separate passive target/quiet block. Keyboard examples
can establish the non-admission warm-start precheck and help freeze the causal
collection trigger, but they cannot alter a passive centroid or final
threshold.

The causal collection trigger and final decoder threshold are different
parameters with different evidence roles. Freeze the collection trigger before
the first passive centroid target. Lock the final threshold after its disjoint
passive block. Held-out evidence can change neither. A policy change increments
the artifact schema or policy identifier; it never reinterprets an existing
artifact.

## Locked validation

After fitting, the coordinator freezes preprocessing, centroids, variance,
final decoder threshold, rejection threshold, and segmentation constants. The
causal collection trigger was frozen earlier and remains a separate parameter.
Validation examples cannot update either trigger, any threshold, or the model.

The UI runs a held-out randomized passive-surface sequence containing:

- every enabled Core-28 key;
- every reserved position;
- a separately shuffled order in each of three rounds;
- repeated same-finger positions;
- deliberate rests; and
- three 22-second scripted ordinary-motion intervals per hand.

The surface-transfer screen remains visible for both fitting and held-out
targets. It labels the active stage and keeps the exact cue and highlighted
position in the rendered DOM. The browser acknowledges a cue only while that
screen is visible and its rendered text still matches the pending snapshot.
The validation-result screen appears only after the held-out report passes or
fails. A hidden result screen can never acknowledge a target.

The persisted status is `ValidationPassed` and
`heldout_validation_passed=true` only when:

- every enabled key has validation evidence;
- every reserved position abstains;
- validation macro-F1 is at least 0.90;
- no enabled class has point recall below 0.80;
- no missing, corrupt, or acquisition-fault frame enters a key decision;
- at least 60 seconds of measured motion-rich, non-contact, non-saturated
  ordinary-motion windows per hand emit no key;
  and
- key state is neutral at the end.

These thresholds are provisional and use point estimates because a short
single-user setup cannot supply decisive confidence intervals. The UI must
label this as held-out validation, not a Gate 2 pass or live authority.

The v2 validation report contains a null-inclusive rectangular confusion
matrix. Its 35 truth rows are the 34 physical positions plus null/ordinary
motion. Its 29 prediction columns are the 28 enabled Core-28 outputs plus null.
Every physical-position truth row requires at least three held-out examples.
Every reserved-position row must predict null. The null row and at least
60,000 ms of measured motion-rich, non-contact, non-saturated ordinary-motion
windows per hand are mandatory. Quiet rest, contact-like windows, saturated
motion, and caller-supplied durations do not count. The engine derives duration
from retained frame intervals. Macro-F1 and minimum recall are derived from the
28 enabled rows; callers cannot submit aggregate scores without the underlying
counts.

The report also binds the locked model-artifact hash, acquisition-manifest
hash, evaluator version, validation session hashes, one derived-evidence
SHA-256 per session in the same order, and validation block IDs. It rejects a
missing, duplicate, or count-mismatched session/evidence binding.
Training/threshold sessions and blocks cannot reappear in validation. Missing,
duplicate, stale-artifact, wrong-manifest, overlapping, or incomplete evidence
fails before the validation disposition is evaluated.

## Progress model

The public calibration snapshot exposes these exact progress and result fields
in addition to device, prompt, lease, and acknowledgement metadata:

```text
phase: disconnected | hardware-check | keyboard-identify | neutral-pose |
       keyboard-collect | keyboard-validate | surface-collect |
       fit-locked-artifact | surface-validate | validation-passed |
       calibration-failed | aborted
screen: connect | sensor-check | identify-keyboard | neutral-pose |
        keyboard-typing | surface-transfer | validation-result
hardware_health: string
elapsed_seconds: u32
keyboard_accepted: u32
keyboard_required: u32
surface_accepted: u32
surface_required: u32
rejected_examples: u32
setup_progress_basis_points: 0..10000
validation: not-run | running | passed | failed
heldout_validation_passed: bool
failure_cause: string | null
```

`surface_accepted` and `surface_required` count fitting targets only. They do
not become held-out counters. During held-out collection the public snapshot
uses `phase=surface-validate`, `screen=surface-transfer`,
`validation=running`, and `setup_progress_basis_points=9900`. The current
schema has no separate numeric held-out progress meter. Add one only through an
explicit public-schema change, not by reinterpreting the fitting counters.

The front-end may draw a progress bar from completed required work. It must
label the bar `Setup progress`, not `Calibration accuracy`. Accuracy appears
only in the saved validation evidence after validation; the current public
snapshot does not expose a numeric accuracy field.

The bar is exactly 99 percent while held-out validation runs. Success publishes
`phase=validation-passed`; failure publishes `phase=calibration-failed`.
Neither state silently authorizes live output. The flow never loops forever.

### Hard cap and plateau rule

**Provisional:** stop adaptive collection after either:

- 1,200 seconds total setup time;
- 20 accepted examples for one key without meeting its collection rule; or
- a disjoint keyboard signal check or locked held-out check that fails its
  fixed rule.

The same failing condition twice across fresh mounts is a hardware or product
blocker, not a request for more user patience. The report names one of:

- unexpected sensor timing or acquisition failure;
- mount movement or orientation failure;
- no separable contact impulse;
- finger ownership inconsistent with the Core-28 manifest;
- same-finger positions not separable;
- keyboard-to-surface transfer failure; or
- unsupported layout or input behavior.

## Nonstandard finger use

The acting finger sensor is observable, so the app can detect repeated conflict
between the canonical touch-typing assignment and the user's actual finger.
The first product does not silently rewrite the Core-28 manifest.

For v1:

- an isolated accidental mismatch is rejected;
- a repeated mismatch is shown by key and observed finger;
- `B` is passive-only and follows the displayed left-index assignment; the
  causal detector inspects both hands and reports a right-hand contact as a
  wrong-hand rejection; and
- the user may retry using the displayed assignment or stop as unsupported.

A later personal-manifest feature may support stable alternate fingering. It
requires a versioned mapping, new layout tests, OS-output tests, and held-out
evidence. It must not become an untracked calibration side effect.

## Artifact and live-authority contract

One successful setup produces an immutable schema-v4 model artifact plus a
persisted validation result. The model artifact is locked before passive
validation and cannot be rewritten with its result. It records both the fitting
capture-journal hash and the hash of the diagnostic evidence derived from that
journal. The second hash is provenance for an output, not another admission
input. The artifact must state that a wear-session confirmation is required.
Neither the artifact nor the persisted result is live-output authority.

The model artifact contains:

- calibration schema, Core-28 layout ID, feature schema, acquisition-manifest
  hash, and prompt-corpus hash;
- source session hashes and immutable block assignments;
- mount IDs; controller, sensor, axis-map, firmware, and host-build identities
  remain bound through the acquisition-manifest hash;
- selected physical keyboard identity and logical layout;
- the explicit layout-confirmation prompt ID, displayed-copy hash, dedicated
  instruction revision, and accepted host time;
- the exact per-sensor mount, wiring, interrupt, and axis bindings plus a hash
  of the derived neutral report;
- clock-fit parameters and residual summary;
- accepted example counts by position and the aggregate rejected-example
  count;
- training-only normalization values;
- the exact keyboard/passive position-training policy;
- intent, finger, position, confidence, and rejection parameters;
- the exact stream-segmentation constants;
- keyboard-domain and passive-domain source roles;
- setup time and creation time; and
- `requires_wear_session_confirmation=true`.

The derived validation report contains the locked validation metrics and
confusion matrix, evaluator identity, validation session identities, validation
blocks, ordinary-motion exposure, final neutral-key state, artifact hash,
manifest hash, the `ValidationPassed` disposition, and
`heldout_validation_passed=true`. It is an inspectable output cache, not an
authority object.

The typed recomputation result is `HeldOutValidation`; a structural, binding,
evidence, or metric failure is `HeldOutValidationError`. Neither type grants
live authority.

The schema-v9 capture journal is the canonical capture input beside the raw
sensor session. It stores facts: prompt IDs, service publication times, browser
acknowledgment times, rendered-state revisions, keyboard events, neutral
windows, and sensor-session identities. It also binds the one canonical fitting
or artifact-bound held-out plan. The plan, not a caller-written label in the
journal, owns each prompt's expected position or null meaning. Complete-plan
admission rejects missing, extra, reordered, relabeled, wrong-purpose, or
wrong-artifact facts before fitting or held-out scoring. Admission also rebuilds
each presentation deadline from the bound plan start, its exact plan offset,
and all prior acknowledged render delay. A merely monotonic but compressed or
shifted timeline is invalid. Preparation, cue, quiet, and motion displays all
participate in this receipt chain.

Each labelled observation owns all 16 source records in its causal window.
Selection and final engine binding both reject intersections within one raw
session. Fitting and held-out sessions have separate ownership sets, and
simultaneous opposite-hand windows remain valid because their source records
are distinct. The decoder may still process held-out frames to make predictions;
only held-out truth metadata stays outside the prediction path.

The service rederives privacy-minimized evidence and the validation report from
the validated plan, schema-v9 journal, clock fits, acquisition manifest, and raw
session. Those derived JSON files are diagnostic outputs only. Before any live
controller or `uinput` output is opened, runtime repeats the held-out derivation
and requires the cached report to equal the recomputed report. The run directory
also keeps fitting and held-out surface-display receipt logs. A prompted
contact's response gate starts at the cue acknowledgment, not the earlier
server deadline. Null and motion windows use the plan interval after the full
acknowledged-delay chain is applied.

No artifact may contain only a model blob. Every value that changes live
behavior must be inspectable or linked to a versioned model and manifest.

The nearest-centroid decoder and calibration coordinator are the inspectable v1
baseline. Any real-data-driven schema change must increment the artifact or
feature-schema version. It must not silently reinterpret an existing artifact.

### Exact user-facing copy

After the held-out result is saved, show:

> Held-out validation passed. The model and evidence were saved. Saved files do not prove that the wearables are still in the same position.

Before each live process can construct output, require this explicit
confirmation:

> Keep both wearables on and in the same position. The system cannot detect removal, movement, or loosening. If either wearable moved, revalidate before typing.

Only after the user confirms may the process show:

> User-attested wear session active. Stop typing before moving or removing either wearable.

On planned stop, fault, or process exit, show:

> Typing stopped and output is neutral. This wear-session confirmation is no longer active. Confirm again before starting a new live process. Revalidate first if either wearable moved.

The confirmation is single-use live authority for that process. Any source,
watchdog, identity, clock, output, or process fault terminally revokes it and
forces neutral output. A surviving or restarted process cannot infer continuity
from sensors, the artifact, the prior report, or prior confirmation. It
rederives held-out validation from the plan-bound capture journal, clock fits,
and raw session, checks the cached report, then requires a fresh confirmation.
If either wearable moved, the user must revalidate before confirming. There is
no silent rearm.

Capture and live transport keep at most 64 pending candidates. A 65th retained
candidate is a terminal typed fault, not an invitation to grow memory or drop a
record. The attempt preserves its first terminal cause, forces neutral output,
and does not replace that cause with a later cleanup, recorder, or UI error.

Calibration checks every completed identity-bound frame before pending-merge
admission, including while fitting or held-out computation runs. Acquisition
flags, sensor validity and skew, sequence continuity, and aggregate and
per-sensor timestamps must remain valid. The first fault closes label and
evidence admission. It also prevents a pending computation result from advancing
the stage.

The incident retains the offending chunk, merge-held candidates, and parser
suffixes with their original receipt times. Later cleanup cannot replace the
first failure. `source_close_complete` reports parser closure;
`retention_complete` separately reports whether every owned input fit the raw
and journal bounds. Identity-matched retained-frame counts remain diagnostic
counts, not evidence authority. A retention failure must not masquerade as a
completely retained incident.

Normal stage handoffs move the per-hand decoders, pending merge state, receipt
horizons, and frame-integrity history to the next worker. The frozen stage owns
the released record prefix; the next worker owns pending candidates and partial
frames, including their earlier bytes. Both handoffs preserve sequence and
sensor-time continuity. A faulted worker cannot transfer authority to a fresh
worker, and a stage change cannot turn an ordinary split read into an
unterminated or malformed frame.

Before background fitting or held-out validation, the coordinator freezes the
stage's raw capture, captured facts, and display receipts. It keeps ownership
independent of the result channel until the normal stage files are durably
saved. A missing passive contact, processing failure, Abort, or lost browser
lease must preserve that original stage as a non-eligible incident. The
replacement integrity monitor is a separate interval and cannot stand in for
the original capture. Save both required intervals before publishing the
terminal `final-state.json`. Incident files grant no calibration or live
authority.

## Privacy and safety

Calibration necessarily observes key labels. The safe default is stricter than
normal typing telemetry:

- serve only on loopback;
- make no network request during a session;
- use only project-supplied prompts;
- disable paste, free composition, and password-field capture;
- show a persistent recording indicator;
- provide an immediate stop control;
- store raw sessions locally with explicit retention;
- never upload raw motion, labels, inferred text, or calibration artifacts;
- redact device serials from exported study reports; and
- release every output key and stop label capture on disconnect, focus loss,
  service exit, or UI abort.

The regular keyboard remains available for Stop because the service does not
grab it. The browser suppresses its own Space and Backspace actions while
capture is active, but browser events never become labels.

Stop setup revokes active capture independently of the last rendered state
revision. Elapsed-time or health updates must not reject an authenticated stop
request. Loopback, token, and origin checks still apply, and a published saved
result stays read-only. A stop arriving after a terminal failure preserves that
failure and its diagnostics while leaving capture paused and its lease revoked.
Other actions retain their own freshness checks.

## Front-end screen contract

The first UI needs seven screens, not a general settings application.

| Screen | Required information and action |
| --- | --- |
| Connect | Left/right controller status, six sensors per hand, firmware/manifest compatibility |
| Identify keyboard | One explicit key press, selected device identity, US QWERTY confirmation |
| Sensor check | Per-sensor health, sample rate, skew, faults, actionable reseat message |
| Neutral pose | Three-second countdown, stillness feedback, no manual angle entry |
| Keyboard typing | Stable prompt line, current character, accepted coverage, immediate Stop |
| Surface transfer | Large preparation/`Tap ... now.` cue for fitting and held-out stages, visible stage label, highlighted screen-only 34-position physical grid, fitting-target count, immediate Stop; held-out collection stays on this screen and does not repurpose the fitting count |
| Validation/result | Terminal pass, failure, or stopped copy; named failure cause, wearable warning, and saved-result acknowledgement state; no confusion matrix or numeric metric panel in the current UI, and never used while a cue awaits a render receipt |

The normal view should not show raw feature vectors, model jargon, or dozens of
settings. An engineering details drawer can expose timestamps, counts, flags,
and artifact hashes.

The app must remain fully keyboard-operable and screen-reader-labelled. The
candidate device cannot be the only way to operate its own calibration UI.

## Implementation state machine

The local coordinator owns this one state graph:

```text
Disconnected
    -> HardwareCheck
    -> KeyboardIdentify
    -> KeyboardLayoutDecision
    -> NeutralPose
    -> KeyboardCollect
    -> KeyboardValidate
    -> SurfaceCollect
    -> FitLockedArtifact
    -> SurfaceValidate
    -> ValidationPassed

Any failure-free state -> Aborted
Terminal failure + Stop -> same failure, capture stopped
Any sensor fault -> NeedsHardwareAttention
Any capped model failure -> CalibrationFailed
User-declared remount -> NeutralPose with a new mount identifier
```

There is no force-pass action. Engineering mode may export evidence from a
failed state but cannot create a passed validation artifact or live authority.

## Verification plan

Hardware-independent tests must cover:

- every Core-28 and reserved position enters prompt coverage;
- keyboard warm start excludes `B`, Space, Backspace, and both outer thumbs;
- adaptive selection terminates and is deterministic for a fixed record;
- rejected samples never advance coverage;
- browser acknowledgment opens a response gate but cannot manufacture contact;
- the causal observation distinguishes correct, absent, wrong-hand,
  wrong-finger, and multiple contact across both hands;
- passive centroid and final-threshold evidence are disjoint;
- the frozen collection trigger remains distinct from the final threshold;
- training and validation block IDs cannot overlap;
- validation never mutates the collection trigger, final threshold, or locked
  artifact;
- a wrong keyboard device cannot label a session;
- repeated, held, overlapping, unknown, and modifier events are handled by the
  Core-28 policy;
- progress never equals accuracy;
- hard caps produce one named failure;
- live output requires a fresh per-process user attestation; and
- abort, fault, or process exit terminally revokes that authority, leaves
  output neutral, closes the session, and cannot silently rearm.

Replay tests must drive the same coordinator with:

- a complete successful synthetic session;
- missing key classes;
- a loose or missing sensor;
- inter-sensor skew;
- a clock reset;
- nonstandard finger use;
- reserved-position activations;
- a keyboard-domain pass followed by a surface-domain failure; and
- a failed validation that additional training cannot repair.

Hardware-dependent tests must measure:

- time to first successful setup;
- accepted versus rejected event rate;
- alignment residual and label error;
- calibration and validation confusion by key and finger;
- keyboard warm-start diagnostics versus the passive-only admitted model;
- full remount performance;
- different physical-keyboard categories;
- different passive surfaces; and
- user comprehension, abort success, and discomfort.

## Delivery state and next order

The hardware-independent path now has a plan-bound schema-v9 capture journal,
clock/session binding, schema-v4 model artifact, derived held-out report,
two-hand evidence derivation, source-bound hardware runner, journal-bound
replay, loopback UI, and live output runtime. The hardware runner writes every
required canonical input and diagnostic output before it can report
`ValidationPassed`.

The next order is physical. Every gate below is planned; none has run:

1. Run Gate 0A stream integrity and a non-armable contact pilot on the one-hand
   falsification rig. The pilot only decides whether to expand; it cannot pass
   the canonical two-hand Gate 0B, create an armable Core-28 artifact, or claim
   exact position identity.
2. Inspect timing, neutral-pose statistics, contact matching, ordinary-motion
   rejection, and keyboard-versus-passive contact-domain shift for that hand.
3. Add the second physical hand only after the one-hand Gate 0A and contact
   pilot justify the cost. Admit the second rig through Gate 0A before Gate 0B.
4. Run the canonical two-hand Gate 0B, Gate 1, and Gate 2 rounds, including the
   two-controller guided calibration and locked held-out Core-28 validation.
5. Audit the disjoint passive centroid, threshold, and held-out blocks without
   changing the frozen collection trigger or model.
6. Use completed studies only to define a new version for a future attempt;
   never adapt the current artifact from its held-out result.

This order keeps the front-end honest. It prevents a polished progress screen
from getting ahead of the only unresolved fact: whether passive-surface key
motion is separable.

## Sources

C1-C5 were accessed 2026-08-09. C6 was checked against the primary ST
datasheet on 2026-08-29.

| ID | Source | Use |
| --- | --- | --- |
| C1 | [emg2qwerty NeurIPS paper](https://proceedings.neurips.cc/paper_files/paper/2024/file/a64d53074d011e49af1dfc72c332fe4b-Paper-Datasets_and_Benchmarks_Track.pdf) | Precedent and boundary for wearable signals labelled while typing on a physical keyboard |
| C2 | [W3C UI Events](https://www.w3.org/TR/uievents/) | Physical code, logical key, down/up, modifier, and repeat distinctions |
| C3 | [Usable text-entry feasibility contract](../research/usable-text-entry-feasibility-contract.md) | Session integrity, causal timing, Gate 2 metrics, calibration-time targets, and split rules |
| C4 | [Wire protocol v2](wire-protocol-v2.md) | Per-sensor timestamps, physical source identity, stream faults, session capture, and fail-neutral rules |
| C5 | [Reference architecture](reference-architecture.md) | Core-28 mapping, passive-surface product boundary, and independent decoder baseline |
| C6 | [ST LSM6DS3TR-C datasheet](https://www.st.com/resource/en/datasheet/lsm6ds3tr-c.pdf) | Direct-register, block-data-update, and pulsed-DRDY source semantics; project sampling and decimation policy remains a design choice |
