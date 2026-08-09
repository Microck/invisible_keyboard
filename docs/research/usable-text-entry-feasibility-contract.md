# Usable text-entry feasibility contract

Date: 2026-08-09

## Decision

An invisible-keyboard prototype is feasible only if it passes a sequence of
end-to-end gates. An isolated gesture score, a balanced-window classifier
accuracy, or language-corrected words per minute cannot substitute for those
gates.

This contract separates three different claims:

1. **A - phrase transcription:** the sensor may identify only a finger or
   uncertain key distribution, and a declared language model may recover an
   English phrase. This can be useful, but the corrected result is evidence for
   phrase transcription only.
2. **B - arbitrary key intent:** the system must recover the actual key-down,
   key-up, modifier, hold, repeat, and chord intent needed for passwords,
   identifiers, code, symbols, navigation, and shortcuts. Language correction
   is disabled for this claim.
3. **C - accessibility or chorded entry:** a learned chord alphabet or an
   alternate keyboard interface may be useful without reproducing spatial
   QWERTY. It needs its own learning and accessibility study and must not borrow
   the throughput or familiarity claims of touch typing.

An architecture may pass A and fail B. That is a valid result, not a partial B
pass. C is optional unless the product claim includes it. The human still must
choose the desired claim and final acceptance levels.

## How to read thresholds

Every requirement below has one of these labels:

- **E - evidence-backed:** the metric, protocol boundary, or behavior comes
  from a primary method paper, standard, or official platform documentation.
- **P - provisional:** an engineering stop/go value chosen to make the first
  prototype falsifiable. It is not a published universal usability boundary.
- **H - human decision:** a product preference that this research cannot set.

Most definitions are E. The false-activation, cross-user, fatigue, battery,
setup, and absolute speed pass lines are P. Final product thresholds are H.

## Non-negotiable evidence contract

### Immutable session data

Before collecting a session, freeze a manifest containing:

- protocol and corpus version plus content hashes;
- participant pseudonym, session ID, date, posture, surface, environment, and
  task condition;
- hardware serials, board and sensor revisions, firmware hash, sampling
  configuration, radio configuration, host build, model hash, and decoder
  settings;
- device placement, don/doff or remount event, calibration inputs, calibration
  time, and whether calibration data can enter training;
- one monotonic device timestamp and sequence counter per stream, host receipt
  time, clock-mapping events, ground-truth time, model-decision time, operating
  system injection time, and application-observed time;
- raw sensor records, raw ground truth, raw decoder hypotheses, operating
  system events, transcription snapshots, corrections, and final text; and
- consent, retained data classes, encryption state, network destinations,
  deletion date, and any camera, audio, biometric, or bystander exposure.

Write raw sessions append-only and hash them before preprocessing. Never repair,
trim, relabel, resample, or discard a raw session in place. Corrections become a
new derived artifact with a transformation log.

### Split before transformation

Assign immutable whole sessions and people to train, validation, and test
groups before windowing, normalization, feature selection, augmentation, or
synthetic balancing. Fit every learned transform on training data only. Adjacent
or overlapping windows from one raw event must never cross a split. Official
ML guidance warns that preprocessing before a split leaks test information, and
inertial HAR research shows that apparent gains from overlapping windows can be
an artifact of subject-dependent validation [S11][S12].

Maintain three separate evaluations:

- **within-session:** later held-out blocks from a session not used to fit the
  model;
- **session-held-out:** a complete later session, including a full remount, not
  used in any tuning; and
- **person-held-out:** complete people absent from training, feature fitting,
  model selection, threshold selection, and corpus adaptation.

If calibration is allowed, report both zero-calibration and calibrated results,
including the exact samples and elapsed setup time. A personalized result is not
a person-transfer result.

### Stream integrity and synchronization

Every packet must be received once, or its absence must be visible through the
sequence counter. Report sent, received, missing, duplicate, reordered, corrupt,
and late records per stream, plus the number and duration of gaps. Report clock
offset, drift, resynchronizations, and residual error. Do not interpolate across
a gap without retaining a gap mask and an ablation against the uninterpolated
stream.

For multiple sensors, evaluate the full topology at once. Testing each sensor
alone cannot establish cross-hand or cross-modality synchronization. A model
window is not an independent sample, and packet loss is not a classification
error. Report both.

## Metric dictionary

Let `P` be presented text, `T` final transcribed text, `I` the ordered physical
or inferred input actions, and `Q` the ordered full-text snapshots after every
text-changing action. A T-sequence is needed because a character-only input
stream cannot represent autocorrection, cursor movement, selection, prediction,
undo, or paste [S4].

### Text speed and completion

- **WPM (E):** `((|T| - 1) / seconds) * 60 / 5`, where time runs from the first
  entered character to the last, and a word is five characters including
  spaces. Report WPM only with error [S3].
- **task time (E):** presentation-to-commit time. This includes orientation,
  mode switching, correction, and hesitation that standard WPM excludes.
- **task completion:** completed trials divided by presented trials. Timeouts,
  aborts, decoder failures, and user requests to restart are failures, not
  missing data.
- **baseline ratio:** invisible-keyboard WPM divided by the same participant's
  physical-keyboard WPM on the same frozen corpus. Counterbalance order.

Transcription and composition are different tasks. Use copying for the gated
comparison. If free composition is studied, report it separately because
thinking time changes the meaning of WPM [S3].

### Text and event errors

- **CER:** `(substitutions + deletions + insertions) / |P|`. This common
  reference-length form can exceed 100 percent. State normalization explicitly.
- **MSD error rate (E):** minimum string distance normalized by the longer
  aligned string, following Soukoreff and MacKenzie. It is bounded and is not
  numerically interchangeable with every CER definition [S1].
- **word error rate:** word-level edit distance divided by reference word count.
  It is secondary because passwords, code, and shortcuts have no useful word
  unit.
- **uncorrected error rate, UER (E):** `INF / (C + INF + IF)`, where `C` is
  correct input, `INF` is incorrect and not fixed, and `IF` is incorrect but
  fixed [S1].
- **corrected error rate (E):** `IF / (C + INF + IF)` [S1].
- **total error rate (E):** `(INF + IF) / (C + INF + IF)` [S1].
- **KSPC (E):** all input actions divided by final transcribed characters.
  Also report correction actions, correction seconds, candidate selections,
  undo operations, and prediction acceptances per output character. KSPC is
  method-dependent and may be below 1 with prediction, so it is not a universal
  cross-method error score [S1].
- **key-event error rate:** edit distance over the ordered expected key events,
  including down and up, divided by expected events. Report insertion,
  deletion, substitution, duplicate-down, orphan-up, and stuck-state counts.
- **missed activation rate:** unmatched intended activations divided by intended
  activations. Report per finger, key, posture, and session.
- **false activations per hour:** unmatched output activations divided by armed
  non-typing hours. Never report only false-positive percentage per balanced
  window. Always-on detectors use exposure-normalized false activations and
  missed-activation proportions because deployment base rates dominate [S15].

Match an intended activation to at most one predicted activation and vice versa.
Use a frozen causal matching window, initially `[-50 ms, +250 ms]` (P), then
report the signed latency of matched pairs. Extra predictions are insertions;
unmatched intentions are misses. Never pick a new window after seeing test
results.

### Identity and continuous recognition

For intent, finger, and key classification report:

- per-class precision, recall, and F1;
- macro-F1 and event-weighted F1;
- a count confusion matrix, including the null or ordinary-movement class;
- top-1 only for B; top-k is a decoder diagnostic, not key recovery;
- calibration and non-calibration results; and
- participant-, session-, surface-, placement-, and environment-level results.

Window accuracy is diagnostic only. The gate uses continuous event matching and
final task behavior.

### Latency, jitter, holds, and rollover

Record these causal intervals separately:

1. intent/contact to first sensor sample;
2. sensor sample to host receipt;
3. host receipt to model decision;
4. decision to operating system injection;
5. injection to application observation; and
6. intent/contact to visible or logged application response.

For each, report median, p90, p95, p99, maximum, and a histogram. Report cold
start, steady state, radio reconnect, correction, and concurrent-input cases.
Model-only mean latency is not end-to-end latency. A 2023 N=31 study comparing
20 ms with 200 ms display latency found that the high-latency condition harmed
text correction even though copy-task performance did not significantly change
[S7]. That supports testing correction separately; it does not establish a
universal 100 ms cutoff.

The B contract additionally requires:

- exactly one key-down and key-up for an ordinary press;
- key-up after every fault or disconnect so no modifier remains stuck;
- hold duration error and detection of an intentional hold;
- repeat start and repeat rate against the current host configuration;
- exact modifier state before, during, and after a chord;
- advertised two-key and multi-key rollover without reordering; and
- lock, dead-key, navigation, function-key, and layout behavior when advertised.

W3C UI Events distinguishes key-down, key-up, repeat, physical `code`, logical
`key`, and modifier state [S8]. Windows accessibility features can change repeat,
acceptance, and bounce timing, while StickyKeys serializes modifiers [S10]. Test
the active platform settings rather than hard-coding one timing assumption.

### Calibration, transfer, setup, and recovery

Report calibration examples and seconds, don/doff time, remount time, failed
setup attempts, retries, manual adjustments, and help required. After a forced
disconnect, sensor removal, occlusion, packet burst loss, host sleep, focus
change, and decoder crash, report:

- false output during failure;
- time to a safe all-keys-up state;
- time to the first correct post-recovery event;
- data loss; and
- whether calibration had to be repeated.

### Learning, comfort, privacy, and battery

- Plot participant-level WPM, UER, total error, corrections, and task completion
  against cumulative practice minutes. Fit a learning curve only as a summary;
  do not treat extrapolated expert speed as observed evidence.
- Record local hand, finger, wrist, forearm, shoulder, and neck discomfort on a
  fixed 0-10 scale before the task and every 15 minutes. Use NASA-TLX after each
  condition for mental, physical, temporal, effort, performance, and frustration
  workload [S16]. A Cornell hand discomfort form may supplement this, but it is
  a screening tool, not a diagnosis [S17].
- Record raw sensor types retained, camera/audio field of view, bystander
  capture, biometric content, inferred text, permissions, storage duration,
  network egress, on-device versus remote processing, and deletion controls.
- Measure steady-state and peak power at the complete wearable and host, radio
  duty cycle, inference duty cycle, energy per hour, tested runtime, projected
  runtime, recharge time, and battery mass. A component data-sheet estimate is
  not a system runtime result.

## Stage-gate protocol

The gates are cumulative. Do not train around a failed gate. Find the physical,
data, or interaction cause, change the design, and rerun that gate with a new
versioned artifact.

### Gate 0 - bench stream integrity

**Protocol:** Run the complete topology for at least 60 minutes, inject at least
10,000 deterministic press/release records, and force 10 disconnect/reconnect or
power-cycle events. For fused sensors, generate shared timing edges visible to
all channels.

**Go:**

- 100 percent of sequence numbers are accounted for (E);
- receipt is at least 99.9 percent, with no silent timestamp reset, corruption,
  duplicate delivery, or reordered delivery to the model (P);
- cross-stream synchronization residual is at most the smaller of 5 ms or 10
  percent of the shortest feature used by the classifier (P);
- end-to-end p95 is at most 100 ms and p99 at most 200 ms (P, with the 200 ms
  correction-risk condition anchored by [S7]); and
- every forced fault reaches all-keys-up within 250 ms, with zero stuck modifier
  or repeat state (P).

**Stop:** Any loss that cannot be detected, any clock ambiguity, any stuck key,
or any metric computed only on received packets without the sent denominator.

### Gate 1 - isolated tap intent

**Protocol:** Use at least 5 participants, two complete mounts per person, and at
least 50 randomized intended activations per finger or supported actuator in
each mount. Use synchronized physical contact, high-speed video, or another
reference independent of the candidate sensor. Include equal-duration quiet and
scripted non-tap blocks.

**Go:** The one-sided 95 percent upper confidence bound for misses is at most 5
percent, p95 intent-to-event latency is at most 100 ms, p99 is at most 200 ms,
and no class has point recall below 80 percent (all P). Report false activations
per hour, but reserve the decisive ordinary-movement test for Gate 8.

**Stop:** Ground truth depends on the same sensor under test, timestamps cannot
be aligned, or balanced isolated windows are the only negative evidence.

### Gate 2 - finger and key identity

**Protocol:** Use at least 8 participants, two mounts, and at least 50 randomized
events per advertised class per mount. Freeze session-held-out splits. For A,
the target may be finger identity. For B, it must be exact key intent with the
language model disabled.

**Go:** The lower 95 percent participant-level confidence bound for macro-F1 is
at least 0.90, the event error upper bound is at most 10 percent, and every
class has point recall of at least 0.80 (P). Show the full null-inclusive
confusion matrix.

**Stop:** Same-finger keys are unobservable while B is claimed, top-k or word
correction replaces top-1 identity, or windows from one event cross a split.

### Gate 3 - live uncorrected text

**Protocol:** At least 10 participants complete two sessions and at least 1,000
reference characters per session. Disable language models, autocorrection,
prediction, and post-hoc smoothing that uses future events. Run one no-correction
block and one natural manual-correction block. Preserve raw events and every
transcription snapshot.

**Go:** On participant-level estimates, the one-sided 95 percent lower bound is
at least 15 WPM and the upper bound is at most 10 percent UER; task completion is
at least 95 percent; Gate 0 latency and key-state conditions still hold (P).

**Stop:** Only offline replay works, a final string cannot be traced to causal
events, or the decoder silently uses phrase knowledge.

### Gate 4 - corrected phrase text (A)

**Protocol:** Lock the language model, vocabulary, decoding beam, and training
corpus before testing. Use both the MacKenzie-Soukoreff 500-phrase set for
lowercase alphabetic comparability and a held-out EnronMobile set for mixed
case, digits, and punctuation [S2][S6]. No test phrase, near duplicate, or user
appears in decoder training. At least 10 participants enter at least 20 phrases
from each set.

**Go:** The lower 95 percent participant-level bound is at least 20 WPM, the
upper bound is at most 3 percent final CER, task completion is at least 95
percent, and manual correction actions are at most 0.15 per output character
(P). Report sensor-only event metrics, pre-language output, final output, KSPC,
correction time, OOV rate, and candidate-selection burden separately.

**Stop:** Only final corrected text is retained, the test corpus trained the
language model, or the result is presented as B evidence.

### Gate 5 - arbitrary text, code, symbols, and shortcuts (B)

**Protocol:** Disable all language correction and prediction. Freeze a corpus
with: balanced randomized key sequences; unseen identifiers, UUID-like strings,
and password-like strings; mixed-case and punctuation-rich EnronMobile samples;
permissively licensed source-code snippets whose repository and commit are
hashed; every advertised symbol and navigation key; and at least 100 randomized
modifier, hold, repeat, and shortcut trials. Include commands with harmless
observable effects in an isolated test application.

**Go:** Every advertised key has evidence; the upper 95 percent key-event error
bound is at most 5 percent; the lower 95 percent shortcut/chord success bound is
at least 95 percent; entry is at least 15 WPM; every ordinary key-down has the
correct key-up; and there are zero stuck or unintended destructive shortcut
states (P). Repeat onset must match the active host delay within 50 ms and repeat
rate within 5 percent (P).

**Stop:** Unsupported keys are hidden from the denominator, text is normalized
before scoring, a dictionary repairs identifiers, or a shortcut is inferred
from the application's later state rather than the emitted event trace.

### Gate 6 - session and remount transfer

**Protocol:** At least 10 participants complete three sessions separated by at
least 24 hours. The final session is held out before preprocessing and begins
with a complete don/doff or camera/surface remount. Run locked zero-calibration
and declared-calibration conditions.

**Go:** Relative to the same person's within-session result, WPM falls by no
more than 20 percent, UER rises by no more than 3 percentage points, and miss
rate rises by no more than 3 points. Median calibration is at most 2 minutes,
p95 at most 5 minutes, and p95 total setup at most 5 minutes (P).

**Stop:** Test-session samples change normalization, thresholds, model weights,
or remount geometry before scoring.

### Gate 7 - person transfer

**Protocol:** Lock architecture, hyperparameters, preprocessing, thresholds,
and all corpora, then evaluate at least 20 wholly unseen people. Report the
demographic, handedness, touch-typing, disability, anatomy, and prior-device
coverage relevant to the intended population. Any allowed personal calibration
is a separate condition.

**Go:** The base A or B gate still passes, and the one-sided 95 percent lower
bound on the fraction of people who individually meet that gate is at least 0.80
(P). Report the worst participant and all exclusions.

**Stop:** A participant enters tuning, poor users are removed as outliers, or a
personalized model is described as zero-shot transfer. A personal-only product
may mark this gate not applicable, but may not claim person transfer.

### Gate 8 - ordinary-movement negatives

**Protocol:** Arm the complete system during at least 30 participant-hours over
at least 10 people. Include desk work, mouse and phone use, object handling,
eating, drinking, scratching, gesturing, walking, transport vibration, resting
hands, conversation, don/doff, and the use-case-specific environment. Randomly
interleave at least 200 intentional activations to measure misses under realistic
base rates.

**Go:** The one-sided 95 percent upper bound is at most 0.1 false activations per
hour, the missed-activation bound remains at most 5 percent, and there is no
false modifier chord or destructive shortcut (P). With zero false activations,
the rule of three requires 30 hours to bound the rate near 0.1/hour [S14].

**Stop:** Negatives are chopped into balanced windows, inactive time is not
logged, activities are selected after inspecting errors, or a wake gesture is
quietly added without redefining the interaction.

For product qualification, test a provisional target below 0.01/hour, which
requires about 300 zero-event hours for the same upper bound. AllSee's reported
0.083/hour over 24 hours used an explicit repetitive unlock gesture, so it is a
useful exposure example but not a commensurate invisible-keyboard threshold
[S18].

### Gate 9 - extended use, learning, and recovery

**Protocol:** At least 12 participants complete counterbalanced 90-minute
candidate and physical-keyboard conditions. Record 15-minute blocks, discomfort,
NASA-TLX, failures, power, and task results. Learned chord systems additionally
need at least 400 minutes over repeated days before any novice learning claim;
Twiddler reached 26.2 WPM after 400 minutes and 47 WPM only in a five-person,
roughly 25-hour continuation [S19].

**Go:** No adverse event; median local discomfort increase is at most 2/10 and
the participant p90 at most 4/10; final-block WPM falls by no more than 10
percent from the first stable block; UER rises by no more than 2 points; task
completion remains at least 95 percent; recovery leaves no stuck state; and
measured or projected battery runtime covers at least 8 hours or the declared
use-case duration (P).

**Stop:** Pain or numbness persists, performance collapses with time, failures
require hidden operator intervention, or battery life is extrapolated from a
sensor data sheet without whole-system power measurement.

This is a feasibility and comfort screen, not evidence of long-term ergonomic
safety. A production safety claim needs a prospectively powered, longer field
study and qualified ergonomics review.

## Sample-size and statistical reporting rules

There is no defensible universal HCI sample size. Lakens identifies accuracy,
prospective power for a smallest effect of interest, resource constraints, and
explicit heuristics as different justifications [S13]. The early gate counts
above are P engineering screens chosen to cover people, classes, remounts, and
continuous exposure. They do not establish population generality.

Use these rules:

1. Name the independent unit. Sensor windows are not independent participants
   or independent events.
2. Report participant and session counts, event counts, characters, phrases,
   and armed negative hours. Do not report only the largest denominator.
3. Aggregate first at participant/session level. Report mean, standard
   deviation, median, interquartile range, individual points, and 95 percent
   intervals. Use a hierarchical bootstrap for participant and session metrics.
4. Use exact binomial intervals for event success or miss proportions and exact
   Poisson intervals for false activations per exposure time. If zero events
   occur, report the upper bound, not `0 percent` [S14].
5. For every gate, pass only when the specified confidence bound clears the
   threshold. A favorable point estimate with a crossing interval is
   inconclusive.
6. Counterbalance within-person comparisons and publish the paired differences.
7. After the pilot estimates variance and cluster correlation, prospectively
   power a confirmatory study for a declared smallest relevant change in WPM,
   UER, false activations, and discomfort. Do not use post-hoc power.
8. Freeze hypotheses, exclusions, matching windows, splits, thresholds, and
   analysis code before the decisive test. Report every deviation.

The 20-person person-transfer gate has a useful property: if all 20 pass, the
lower exact confidence bound is above roughly 80 percent. Any failures widen the
uncertainty quickly, which is the intended signal to collect more people rather
than generalize from event count. The 30- and 300-hour negative tests follow
directly from the rule of three when no false activations occur. Correlated
events, environments, or repeated movements reduce effective coverage, so
diversity remains mandatory.

## Published numbers that are not commensurate

| Claim | Why it cannot set this contract's threshold |
| --- | --- |
| TypeAnywhere: 70.6 WPM, 1.50% CER on a table | Five-day English phrase transcription from finger identity plus a language model; same-finger key identity is not sensed; the paper also has a cohort-count inconsistency. It is A, not B. |
| TapType: 19.2 WPM, 0.6% CER | Online N=10 after personal calibration and about 30 minutes, hard-surface wrist impacts, Bayesian plus n-gram decoding. It is A and does not report decisive ordinary-movement exposure. |
| StegoType: 42.4 WPM, 7% UER | Online N=18, no language model, 30 Hz Quest 3 cameras, large multi-person training corpus. It is closer to B phrase evidence, but headset, tracker, surface, cohort, and UER differ from the wrist systems. |
| OptiTrack invisible typing: 73 WPM, 2.38% UER | Offline marker-glove and multi-camera laboratory upper bound, not a commodity live path. |
| mmKey: >95% single-key, >90% multi-key, >97% word recognition | Isolated key/word recognition on a custom unavailable radar array; commercial correction; no comparable live WPM/UER or always-on test. |
| emg2qwerty: about 6.95% personalized CER | Offline physical-keyboard labels, research 32-channel sEMG, personal fine-tuning; generic unseen-user CER was about 51.8%. It does not prove keyless operation. |
| PinchType: 12.54 WPM including correction, 20.07 excluding it | Different thumb-to-finger motor action and a large correction-time boundary. The two speeds demonstrate why correction cannot be omitted. |
| QwertyRing: 13.74 to 20.59 WPM | One-finger orientation plus trigram decoding, five-day learning, no OOV entry, and >1 cm raw position error. It is not ten-finger B. |
| Twiddler: 26.2 WPM at 400 minutes, 47 WPM near 25 hours | Physical one-handed chord keys, letter-only standard phrases, self-selected five-person continuation for the expert study. It informs C learning exposure only. |
| AllSee: 0.083 false activations/hour | Eight coarse gestures, five or fewer users, 24-hour exposure, and a repetitive access gesture. The exposure metric is useful; the interaction and error cost differ. |
| Physical keyboard or phone baselines | Different hardware, task, corpus, correction policy, language, participant skill, and timing boundary. Use a paired same-person, same-corpus baseline instead. |

These boundaries come from the prior-art and sensing reports, whose primary
sources are linked there. Do not pool their headline WPM, CER, accuracy, or
latency values into one ranking.

## Prototype scorecard template

Copy this for every immutable build and link every result to raw artifacts.

```markdown
# Invisible keyboard scorecard

## Claim and build
- Claim under test: A / B / C
- Architecture:
- Hardware and firmware hashes:
- Host, OS, injection path, application:
- Model and decoder hashes:
- Supported key/event set:
- Explicitly unsupported behavior:

## Data contract
- Protocol and corpus hashes:
- Raw session manifest:
- Train/validation/test assignment made before windowing: yes/no
- Participants / sessions / remounts:
- Intended events / characters / phrases:
- Armed negative hours and activity coverage:
- Calibration allowed and measured:
- Privacy, retention, and network-egress record:

## Gates
| Gate | Claim | Target and label | Result with 95% interval | Artifact | Pass/fail/inconclusive |
| --- | --- | --- | --- | --- | --- |
| 0 stream integrity | all | see contract | | | |
| 1 isolated intent | all | see contract | | | |
| 2 finger/key identity | A/B | see contract | | | |
| 3 live uncorrected text | A/B | see contract | | | |
| 4 corrected phrase text | A | see contract | | | |
| 5 arbitrary text/code | B | see contract | | | |
| 6 session/remount transfer | claimed | see contract | | | |
| 7 person transfer | claimed | see contract | | | |
| 8 ordinary negatives | all | see contract | | | |
| 9 extended use | intended use | see contract | | | |

## Required metric panel
- WPM, task time, completion, and paired physical-keyboard ratio:
- CER/MSD, UER, corrected and total error, WER:
- KSPC, correction actions/character, correction seconds:
- Misses, substitutions, insertions, false activations/hour:
- Per-class precision/recall/F1 and null-inclusive confusion matrix:
- End-to-end latency p50/p90/p95/p99/max and stage breakdown:
- Down/up, hold, repeat, modifier, rollover, stuck-state results:
- Packet receipt/loss/duplicate/reorder/gaps and sync residual:
- Calibration, setup, remount, and recovery time:
- Within-session, session-held-out, and person-held-out results:
- Learning curve by cumulative practice minutes:
- Discomfort, NASA-TLX, failure and task-completion results:
- Measured power, runtime, recharge, and battery mass:
- Privacy/setup burden and bystander/network exposure:

## Boundary and decision
- Language model/correction boundary:
- Conditions not represented:
- Non-commensurate prior claim accidentally tempting comparison:
- Failed or inconclusive gate and falsified assumption:
- Next smallest experiment, or stop reason:
```

## Implications for remaining tickets

- **Verify operating-system input and compute paths:** the OS-path ticket must
  prove application-observed down/up, logical versus physical key identity,
  modifiers, repeats, rollover, accessibility settings, focus changes, sleep,
  disconnect, and safe all-keys-up recovery. A host API call alone is not proof.
- **Select the sensing architecture:** compare options against the earliest gate
  they can physically satisfy. Finger-only IMU paths can target A; B requires an
  evidenced geometry or instrumented-location channel. Architecture choice must
  price the negative-exposure, remount, privacy, and synchronization tests, not
  only the sensor BOM.
- **Choose the decoding strategy:** freeze separate sensor-only, causal
  key-intent, and language-corrected outputs. A decoder may optimize A without
  turning that result into B. Training, calibration, and evaluation units must
  be complete sessions or people before any derived windows exist.
- **Specify the decisive prototype:** implement Gate 0 first, then the earliest
  claim-specific gate. Do not build a polished keyboard service until stream
  integrity, continuous intent, and ordinary negatives are measurable. The
  scorecard is the prototype's acceptance artifact.

## Sources

All web sources were accessed 2026-08-09. Grades use I for peer-reviewed
research, II for standards and official technical documentation, and VI for
community evidence. Community reports informed negative activities only and do
not set thresholds.

| ID | Grade | Source | Use |
| --- | --- | --- | --- |
| S1 | I | [Soukoreff and MacKenzie, Metrics for Text Entry Research](https://www.yorku.ca/mack/chi03.html) | MSD, KSPC, corrected, uncorrected, and total error definitions |
| S2 | I | [MacKenzie and Soukoreff, Phrase Sets for Evaluating Text Entry Techniques](https://www.yorku.ca/mack/chi03b.html) | 500-phrase properties, lowercase/punctuation boundary, controlled transcription |
| S3 | I | [Wobbrock, Measures of Text Entry Performance](https://faculty.washington.edu/wobbrock/pubs/text-07.pdf) | WPM formula and timing, composition boundary, error and action-rate cautions |
| S4 | I | [Zhang and Wobbrock, Beyond the Input Stream](https://faculty.washington.edu/wobbrock/pubs/uist-19.01.pdf) | Full transcription snapshots for modern correction and editing behavior |
| S5 | I | [Zhang, Zhai, and Wobbrock, Text Entry Throughput](https://faculty.washington.edu/wobbrock/pubs/chi-19.03.pdf) | Speed-accuracy tradeoff and requirement to retain component metrics |
| S6 | I | [Vertanen and Kristensson, Genuine Mobile Email Dataset](https://pokristensson.com/pubs/VertanenKristenssonMobileHCI2011.pdf) | Mixed case, digits, punctuation, memorability, and phrase-set domain boundary |
| S7 | I | [Schmid et al., Effects of Text Input Latency](https://hci.uni-regensburg.de/publications/effects_of_text_input_latency_on_performance_and_task_load_2024) | N=31 comparison of 20 ms and 200 ms, correction-specific impact |
| S8 | II | [W3C UI Events](https://www.w3.org/TR/uievents/) | Key-down, key-up, repeat, code, key, and modifier semantics |
| S9 | II | [WCAG 2.2 Understanding SC 2.1.1 Keyboard](https://www.w3.org/WAI/WCAG22/Understanding/keyboard) | Alternate keyboard interfaces, timing independence, modifier-lock compatibility |
| S10 | II | [Microsoft Windows Accessibility Structures](https://learn.microsoft.com/en-us/windows/win32/winauto/accessibility-structures) | FilterKeys repeat/accept/bounce and StickyKeys sequential modifiers |
| S11 | II | [scikit-learn Common Pitfalls](https://scikit-learn.org/stable/common_pitfalls.html) | Split before learned preprocessing and avoid test leakage |
| S12 | I | [Dehghani et al., Overlapping and Non-Overlapping HAR Windows](https://pmc.ncbi.nlm.nih.gov/articles/PMC6891351/) | Subject-independent validation and misleading overlap gains |
| S13 | I | [Lakens, Sample Size Justification](https://doi.org/10.1525/collabra.33267) | Inferential-goal, accuracy, power, and resource-based sample rationale |
| S14 | I | [Hanley and Lippman-Hand, Interpreting Zero Numerators](https://jhanley.biostat.mcgill.ca/c607/ch08/zero_numerator.pdf) | Rule of three and upper confidence bounds for zero false activations |
| S15 | II | [Apple, Hey Siri On-device Voice Trigger](https://machinelearning.apple.com/research/hey-siri) | Always-on false-activation/hour and false-reject tradeoff terminology |
| S16 | II | [NASA Task Load Index](https://www.nasa.gov/human-systems-integration-division/nasa-task-load-index-tlx/) | Six workload dimensions and official instrument |
| S17 | II/I | [Cornell Musculoskeletal and Hand Discomfort Questionnaires](https://ergo.human.cornell.edu/ahmsquest.html) | Hand discomfort screen, scoring, and non-diagnostic limit |
| S18 | I | [Kellogg, Talla, and Gollakota, AllSee](https://homes.cs.washington.edu/~gshyam/Papers/allsee.pdf) | Exposure-normalized false positives and wake-gesture boundary |
| S19 | I | [Lyons, Plaisted, and Starner, Expert Chording on Twiddler](https://faculty.cc.gatech.edu/~thad/p/030_10_MTE/twiddler-iswc.pdf) | Observed chord-learning exposure and self-selection boundary |

## Confidence and unresolved choices

Confidence is high in the metric definitions, data-split rules, event-semantics
contract, exposure-normalized false activation reporting, and the need to keep
A, B, and C separate. Confidence is medium in the proposed order and minimum
scope of the gates. Confidence is low in every universal numeric usability
threshold because no published system matches the same claim, corpus,
correction policy, population, environment, and operating-system behavior.

The provisional thresholds make a prototype falsifiable. They do not answer the
human questions about first setting, acceptable hardware, target speed, privacy,
calibration, learning time, budget, or whether A, B, or C is the product.
