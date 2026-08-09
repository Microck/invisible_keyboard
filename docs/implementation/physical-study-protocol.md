# Physical study protocol

Status: normative operator protocol for the physical-study engine in
[`src/physical_study.rs`](../../src/physical_study.rs). The executable contract
tests are in
[`tests/physical-study-contract.rs`](../../tests/physical-study-contract.rs).

The library and CLI contracts are implemented. The three commands in this
document appear in `invisible-keyboard --help` and are the only supported
planning, sealing, and scoring path. Do not replace them with an ad hoc
converter.

## Purpose and claim boundary

This protocol decides whether physical observations support three successive
feasibility claims:

| Gate | Question | Candidate output under test |
|---|---|---|
| Gate 0B | Does the frozen contact detector observe intentional finger contacts at acceptable timing, without extra or false contact events? | Contact events only |
| Gate 1 | Can a fixed reference classifier identify the acting finger while abstaining during required negative conditions? | `thumb`, `index`, `middle`, `ring`, `pinky`, or no output |
| Gate 2 | Can a fixed reference classifier identify every Core-28 physical position and abstain on all six reserved positions? | One of 28 enabled physical labels, or no output |

Gate 0B consumes events from an already-frozen contact detector. Gate 1 keeps
that detector frozen and tests five-finger identity. Gate 2 tests the complete
34-position physical grid. The Gate 2 model has all 34 physical classes, but
only the 28 enabled positions may produce output. These six prompted physical
positions must produce no output:

- `left-outer-thumb`
- `comma`
- `period`
- `semicolon`
- `slash`
- `right-outer-thumb`

Run the gates in order. A later gate does not repair a failed earlier gate.
The analysis proves deterministic processing, provenance binding, split
discipline, and the declared observations. It does not prove that the operator
performed the physical protocol, that a mount was real, or that an independent
truth instrument was accurate.

## Prerequisites

Before freezing a study round:

1. Complete the applicable Gate 0A acquisition and evidence checks on the
   exact controller, firmware, rig, and mount design that the study will use.
   The physical-study scorer does not enforce this prerequisite.
2. Freeze the contact detector artifact and record its canonical lowercase
   SHA-256. Its declared fit ownership must be a nonempty, sorted, unique subset
   of `train` and `validation`. It must never include `heldout`.
3. For Gate 1 or Gate 2, freeze the observation representation and feature
   transform before heldout collection. A derived representation requires a
   canonical transform SHA-256 and a nonempty, sorted, unique fit-split list
   that excludes `heldout`. A raw representation forbids both fields.
4. Select an independent truth source for every planned row. The supported
   sources are a contact switch, high-speed video, an instrumented keyboard,
   or an optical tracker. Candidate-sensor inference is not an accepted truth
   source.
5. Establish a truth timeline and clock-mapping procedure. Each collected row
   records the timeline ID and the SHA-256 of its mapping artifact. A valid hash
   proves identity, not clock accuracy.
6. Assign stable pseudonymous participant, session, rig, controller, mount,
   surface, prompt, and round IDs. Each ID must be nonempty, at most 128
   characters, and contain no control characters.
7. Obtain consent for wearable sensing and for the selected truth instrument.
   High-speed video and motion traces can be identifying data.
8. Prepare append-only study storage. `physical-study-seal` syncs each append
   and syncs the parent directory when it creates the evidence file. Storage
   hardware and later copies remain outside that filesystem durability claim.

Build the operator binary and run software validation in a Crabbox. Copy the
resulting fixed binary and its identity to the physical capture host. Physical
collection is the only part that must run where the prototype and truth
instrument are attached.

## Freeze the round before collection

The round has three distinct artifact layers:

1. A planning roster supplies study-level settings and one expected prompt row
   per trial or negative interval.
2. The split manifest freezes the exact expected row identities.
3. The analysis plan freezes the split-manifest hash, pipeline, methods, and
   thresholds.

Do not present a prompt or collect an observation until the split manifest and
analysis plan exist as immutable bytes.

### Study-level roster settings

The planning input must declare one value for each of these fields across the
round:

- `gate`
- `round_id`
- `contact_detector_sha256`
- `contact_detector_fit_splits`
- `observation_representation`
- `transform_sha256`, when the representation is derived
- `transform_fit_splits`, when the representation is derived

Together, the detector and observation fields form the frozen pipeline. Every
evidence row must repeat that pipeline exactly. The analysis plan binds it to
the round. Changing a detector, transform, representation, or fit-split owner
requires a new round and new frozen artifacts.

### Expected row fields

Each roster row freezes exactly these fields:

- `participant_id`
- `session_id`
- `rig_id`
- `controller_id`
- `mount_id`
- `surface_id`
- `hand`
- `split`
- `condition`
- `prompted_label`
- `truth_source`
- `prompt_id`
- `prompt_ordinal`

The emitted split manifest contains its schema, gate, round ID, and the sorted
set of these expected rows. Duplicate expected rows are forbidden.

The roster does not freeze measured event times, the independent truth result,
candidate observations, observation-artifact hashes, timeline identity, or
clock-mapping hashes. Collection supplies those facts. For an intentional tap,
the independent `truth_label` must equal the frozen `prompted_label`. The
software checks equality, but only the operator and truth instrument establish
that the label is honest.

### Participant-disjoint split construction

Assign each real person to exactly one of `train`, `validation`, or `heldout`
before collection. Do not give the same person a new ID in another split.

The scorer rejects any of these crossings:

- one participant ID across splits
- one session ID across splits
- one `(participant_id, mount_id)` pair across splits
- one observation-artifact SHA-256 across splits

Within a session, participant, mount, rig, surface, and timeline ownership must
remain fixed. A session may name only one controller per hand. Event IDs must
be unique across the complete evidence artifact.

For Gate 0B, heldout evidence is required. Train and validation rows are not
required because Gate 0B does not fit a classifier. The frozen detector must
still declare non-heldout fit ownership.

For Gate 1 and Gate 2:

- `train`, `validation`, and `heldout` must all be nonempty;
- every physical class must appear in both train and validation truth rows;
- training must yield at least one matched candidate event for every class;
- centroids use train only;
- the abstention threshold uses validation only; and
- heldout labels and observations must not influence either choice.

One row is not a recommended sample size for train or validation. It is only
the engine's minimum class-presence rule. The v1 plan does not encode a larger
minimum, so do not claim that it does.

### Frozen artifact bytes and hashes

The manifest and plan use canonical pretty JSON with one trailing newline when
the library writes them. Evidence rows bind the SHA-256 of the exact supplied
manifest bytes and exact supplied plan bytes. Reformatting either file after
its hash has entered a row invalidates the round.

The analysis plan must equal the canonical v1 plan. The following settings are
not operator-tunable:

| Setting | Frozen v1 value |
|---|---:|
| Event match window | 50,000 microseconds before through 250,000 microseconds after truth, inclusive |
| Minimum heldout events per physical class per required mount group | 50 |
| Minimum negative exposure per required condition | 60,000,000 microseconds |
| Minimum heldout participants, Gate 0B / Gate 1 / Gate 2 | 5 / 5 / 8 |
| Minimum heldout mounts per participant | 2 |
| Both hands required | true |
| Minimum contact or output latency | 0 microseconds |
| Participant-balanced p95 latency limit | 100,000 microseconds |
| Participant-balanced p99 latency limit | 200,000 microseconds |
| Miss upper limit | 50,000 ppm |
| Event-error upper limit | 100,000 ppm |
| Minimum physical-class recall | 800,000 ppm |
| Minimum participant macro F1 | 900,000 ppm |
| One-sided alpha | 50,000 ppm, or 0.05 |
| Participant bootstrap replicates | 20,000 with fixed internal seeds |
| Classifier | Training-only nearest centroid with squared distance |
| Threshold | Validation correct-class distance p95, capped below the nearest validation negative distance when separable |

A changed threshold, method, or classifier needs a new schema implementation.
Editing a v1 plan is rejected even if every row is resealed against the edit.

## Required heldout collection

Coverage is checked per heldout participant and physical remount.

| Gate | Required heldout coverage |
|---|---|
| Gate 0B | At least 5 participants and 2 mounts each. For each participant, mount, and hand: at least 50 intentional taps for each of the 5 fingers; at least 60 seconds each of quiet and scripted motion; quiet and scripted-motion exposure must be exactly equal. |
| Gate 1 | At least 5 participants and 2 mounts each. For each participant, mount, and hand: at least 50 intentional taps for each of the 5 fingers; at least 60 seconds each of quiet, shared motion, physical-keyboard typing, desk tapping, and simultaneous ambiguous activity. |
| Gate 2 | At least 8 participants and 2 mounts each. For each participant and mount: at least 50 intentional taps for each of all 34 physical positions. No negative or ambiguous condition is accepted. |

This yields minimum heldout positive counts of 5,000 rows for Gate 0B, 5,000
rows for Gate 1, and 27,200 rows for Gate 2. Gate 0B and Gate 1 coverage is
separate for the left and right hand. Gate 2 covers both hands by requiring all
34 hand-owned positions in every mount.

A second mount ID means a real removal and reapplication of the wearable. Do
not rename an unchanged mount. The engine reports participant and remount
slices, but it cannot observe the physical removal. Surface diversity is
reported and not a v1 pass criterion. Prior participant practice or exposure is
also not represented by the schema. Do not claim a naive-participant result
unless a separate approved protocol establishes that fact.

## Truth intervals and event semantics

### Intentional taps

An intentional tap row must have:

- condition `intentional-tap`;
- a prompted label;
- an equal independently established truth label;
- a truth event time;
- at least 50,000 microseconds of truth interval before that event;
- at least 250,000 microseconds of truth interval after that event; and
- monotonic prompt, truth-start, truth-event, and truth-end times.

Both matching-window endpoints must be representable as unsigned 64-bit
microsecond timestamps: truth-event minus 50,000 and truth-event plus 250,000.
Admission rejects a correctly sealed row if either endpoint underflows or
overflows, or if its truth interval omits any part of that complete window.
Clipping the window to zero or the maximum timestamp is not allowed. Exact
complete endpoints are eligible, and candidate matching includes both endpoints.

Gate 0B and Gate 1 labels are `thumb`, `index`, `middle`, `ring`, or `pinky`.
Gate 2 labels are exact labels from the 34-position manifest and must belong to
the row's declared hand.

### Negative and ambiguous intervals

Every condition other than `intentional-tap` is a negative interval. It must
have positive duration, no truth event, no prompted label, and no truth label.
The simultaneous-ambiguous condition is negative by design. Do not choose a
winner after seeing sensor output.

Gate 2 reserved-position trials are not negative intervals. They are
intentional, positively prompted physical contacts with independent truth.
Their correct logical result is no output.

### Candidate observations

Every candidate event must:

- have a nonempty event ID of at most 128 characters with no controls;
- use a globally unique event ID;
- fall inside its row's truth interval; and
- appear in strictly increasing event-time order within the row.

Gate 0B may use an empty feature vector. Gate 1 and Gate 2 require a nonempty
feature vector on every event. All vectors in a Gate 1 or Gate 2 round must
have one common dimension, with at most 4,096 values.

The match window is inclusive. It starts 50 milliseconds before independent
truth and ends 250 milliseconds after it.

For Gate 0B, the first candidate in that window is the matched contact. Every
other candidate in the positive row is extra. If no candidate matches, the row
is a miss and every candidate is extra. Every candidate in a negative row is
both a false activation and an extra event.

For Gate 1 and Gate 2, the frozen classifier first maps each candidate to an
enabled output or abstains. The first non-abstained output in the truth window
is primary. Other non-abstained outputs are extra. A positive enabled position
with no primary output is a miss. A negative Gate 1 row or reserved Gate 2 row
expects null, so any enabled output is a false activation. A candidate that the
model classifies as a reserved Gate 2 class produces no output.

Gate 1 and Gate 2 count at most one event-error trial per row. A row is an
event error when its primary enabled output differs from the expected enabled
label or null, or when the row contains any extra enabled output. The extra and
false-activation metrics still count individual enabled output events.

Latency is signed candidate time minus truth time. Any negative latency fails
the zero-tolerance minimum-latency criterion, even if aggregate percentiles are
otherwise favorable.

## Collect and seal one row at a time

For each session, collect prompts in contiguous ordinal order starting at zero.
Prompt times must be strictly increasing and truth intervals must not overlap.

For each frozen expected row:

1. Confirm the planned participant, rig, controller, mount, surface, hand,
   condition, prompt, and truth source before showing the prompt.
2. Start the declared truth timeline and observation artifact.
3. Present the prompt once.
4. Record independent truth without consulting candidate output.
5. Record every detector candidate in the interval, including misses, extras,
   and apparent false activations. Do not trim events to improve the result.
6. Finalize the observation artifact and compute its canonical lowercase
   SHA-256.
7. Complete the evidence row with the frozen manifest and plan hashes, frozen
   pipeline, truth clock, observation source, and candidate events.
8. Leave `row_sha256` empty and seal the final row immediately.
9. Write exactly one compact JSON object plus newline to the round's JSONL.
   Never edit or replace an appended row.

Use one finalized observation artifact per row. A shared multi-row artifact
cannot have its final hash when the first row ends, so it cannot satisfy this
protocol's immediate-seal rule.

The semantic row hash is the SHA-256 of the row's JSON representation with
`row_sha256` set to an empty string. The scorer recomputes it. The complete
report also binds the exact JSONL byte hash, the order-independent canonical
row-set hash, per-split row-set hashes, and every declared observation-artifact
hash.

A genuine miss is a valid row with no matching candidate. Seal it. A false
activation or duplicate is also evidence. Repeating either trial would create
optional stopping and is forbidden.

## CLI workflow

The implemented command surface is:

```text
invisible-keyboard physical-study-plan <roster.jsonl> <manifest.json> <plan.json>
invisible-keyboard physical-study-seal <row.json> <row.jsonl>
invisible-keyboard physical-study-score <evidence.jsonl> <manifest.json> <plan.json> <report.json>
```

All three commands use bounded reads and reject an artifact that exceeds the
engine-owned size contract before parsing it.

| Artifact | Hard byte limit | Measured minimum Gate 2 artifact |
|---|---:|---:|
| Evidence JSONL | 268,435,456 | 45,094,554 |
| One evidence row | 1,048,576 | 1,701 maximum row |
| Split manifest | 67,108,864 | 14,213,769 |
| Analysis plan | 65,536 | 1,422 |
| Analysis report | 16,777,216 | 35,595 |

The limits apply to exact encoded bytes. They leave measured headroom without
permitting an unbounded allocation. The row limit is also checked for every
JSONL record before that record is parsed. Canonical output encoding enforces
the same artifact-specific limits.

### Plan

`physical-study-plan` accepts JSONL rows shaped as complete unsealed
`PhysicalStudyEvidenceRow` values. The measured fields are placeholders at
this stage. Planning uses only the expected-row identity fields and the first
row's frozen pipeline. It validates that the roster describes one gate and
round, sorts and rejects duplicate expected rows, writes the canonical split
manifest, computes its exact byte hash, and writes the canonical v1 analysis
plan bound to that hash and pipeline. It does not validate or interpret truth
or candidate observations because collection has not started.

The manifest and plan paths must differ, must differ from the roster path, and
must not exist. The command builds both byte artifacts before opening either
output. It creates both outputs with no-overwrite semantics, syncs both files,
and syncs their parent directories. If any returned create, write, or sync step
fails, it removes any file that this invocation created so it does not leave
one authoritative half of a failed pair. No filesystem can crash-atomically
publish two arbitrary paths, so archive the successful pair together before
the first prompt.

Archive the input roster, manifest, and plan together. Record their exact byte
hashes before the first prompt.

### Seal

`physical-study-seal` accepts exactly one complete, parseable row whose
`row_sha256` is empty. It computes the semantic row hash, encodes one compact
JSON object plus newline, and appends it directly to the named evidence JSONL.
It creates the evidence file when absent. An existing nonempty evidence file
must end in newline or the command refuses to append. Before inspecting that
state, the command takes an exclusive operating-system file lock and holds it
through the append and sync. Concurrent `physical-study-seal` processes that
target the same file therefore serialize against the latest durable length.
All writers must cooperate by using this command; a direct or non-locking
writer violates the protocol.

The command rejects an append when the existing exact length plus the canonical
row length would exceed the 268,435,456-byte evidence limit. It makes this
check while holding the lock and before writing any row byte. After a permitted
append it syncs the file and, for a newly created file, its parent directory.
Full semantic validation occurs during scoring. The seal step does not infer
truth, drop candidates, retime an event, or fill a missing observation.

Once append starts, the command never truncates or rolls back. A short write,
sync failure, process loss, or storage failure can therefore leave a partial
final row. Preserve it. That visible partial append invalidates the round and
must not be trimmed or replaced.

### Score

`physical-study-score` reads the exact evidence JSONL, split manifest, and
analysis plan through their bounded engine decoders. Before reading the first
evidence byte, it takes a shared operating-system lock on the evidence file.
It holds that lock through analysis and durable report publication, so a
cooperating `physical-study-seal` writer cannot change the scored snapshot
between the read and the report sync. All evidence writers must use the same
locking contract; a direct or non-locking writer violates the protocol.

The scorer validates every row, exact roster equality, byte-hash bindings,
identity consistency, split isolation, coverage, and model ownership before
scoring heldout. The report path must differ from every input and must not
exist. Only a successful analysis writes and durably syncs canonical report
JSON with one trailing newline. A validation error produces no report or gate
disposition. An append after scoring releases its shared lock is allowed. It
creates a later pathname state; the existing report continues to identify
only the exact JSONL bytes recorded in its source hash.

For Gate 1 and Gate 2, scoring fits centroids from train, selects the threshold
from validation, then evaluates heldout. Gate 0B evaluates heldout detector
events directly and emits no frozen classifier model.

Threshold selection requires at least one validation event whose nearest
centroid is its correct class. When a validation negative has zero distance to
a centroid, no nonnegative threshold can separate it. The engine then freezes
an abstain-all threshold and lets the heldout identity criteria fail closed.

## Participant-balanced confidence rules

Only heldout rows determine criteria and disposition. The report gives each
heldout participant equal weight, regardless of that participant's row count.
Pooled per-class counts and confusion matrices remain descriptive outputs.

### Miss and event-error rates

- Compute each participant's raw miss rate over intentional trials.
- For Gate 1 and Gate 2, compute each participant's event-error rate over all
  that participant's heldout rows.
- The point estimate is the arithmetic mean of participant rates, converted to
  ppm with downward rounding.
- Compute a one-sided 95 percent Clopper-Pearson upper bound within each
  participant.
- Resample participant bounds with replacement for 20,000 deterministic
  replicates.
- The reported upper boundary is the 95 percent bootstrap order statistic of
  the replicate means, rounded upward to ppm.

Gate 0B uses the miss criterion at 50,000 ppm. Gate 1 and Gate 2 use the
event-error criterion at 100,000 ppm.

### Minimum physical-class recall

- Compute recall for every physical class within every participant.
- Apply a one-sided Clopper-Pearson lower bound within each participant and
  class, using Bonferroni alpha `0.05 / physical_class_count`.
- For the point, average raw participant recalls separately by class, then
  select the lowest class mean.
- For the confidence boundary, resample participants 20,000 times, average the
  bounded recalls by class in each replicate, and retain that replicate's
  lowest class mean.
- Report the lower 5 percent bootstrap order statistic, rounded downward to
  ppm.

Physical class count is 5 for Gate 0B and Gate 1, and 34 for Gate 2. Gate 2
therefore includes reserved-position abstention recall in this criterion. The
required lower boundary is 800,000 ppm.

### Participant macro F1

Gate 1 and Gate 2 compute macro F1 within each participant, then average
participants equally. Gate 1 includes the five finger outputs. Gate 2 includes
the 28 enabled positions, not the reserved positions or null.

For each participant and reported class, the conservative lower calculation
uses one-sided Clopper-Pearson lower bounds for precision and recall with
Bonferroni alpha `0.05 / (2 * reported_class_count)`. It takes their harmonic
mean per class, then the participant macro mean. The report resamples these
participant lower values 20,000 times and uses the lower 5 percent bootstrap
order statistic. The required lower boundary is 900,000 ppm.

### Latency

- Sort matched signed latencies within each participant.
- Compute nearest-rank p95 and p99 for each participant.
- Before participant resampling, compute an exact one-sided,
  distribution-free upper confidence order statistic for each participant's
  population quantile.
- If every participant has the empirical percentile and a finite upper order
  statistic, the point is the upward-rounded arithmetic mean of participant
  percentiles.
- If any participant lacks either value, the report marks the aggregate point
  and bound unavailable and the latency criterion fails closed.
- Resample the per-participant upper bounds 20,000 times and use the upper 95
  percent order statistic of replicate means.

The p95 upper boundary must be at most 100,000 microseconds. The p99 upper
boundary must be at most 200,000 microseconds. The global minimum signed
latency must also be at least zero.

### Exact zero-tolerance criteria

These criteria use observed point counts, not confidence intervals:

- Gate 0B: zero extra contact events and zero negative false activations.
- Gate 1: zero extra output events and zero negative false activations.
- Gate 2: zero extra output events and zero enabled outputs on reserved
  positions.

The negative false-activation metric is reported in milli-events per hour from
the pooled declared negative exposure. Its threshold is exactly zero, so any
false activation fails.

## Read the disposition

Each bounded criterion follows one rule:

- If the point estimate is already outside the threshold, the criterion is
  `fail`.
- If the point is favorable but its confidence boundary crosses the threshold,
  the criterion is `inconclusive`.
- If both the point and confidence boundary clear the threshold, the criterion
  is `pass`.

Each zero-tolerance or minimum-latency point criterion is either `pass` or
`fail`.

All thresholds are inclusive. An `at-most` value equal to its threshold passes,
as does an `at-least` value equal to its threshold.

The overall disposition is:

- `no-go` if any criterion fails;
- `inconclusive` if none fails and at least one is inconclusive; or
- `go` only if every criterion passes.

Do not collapse `inconclusive` into `no-go`, and do not present it as a weak
pass. Collecting more data after inspecting an inconclusive result would break
the frozen roster. A larger follow-up is a new round with a new roster and
plan.

## Report and evidence handling

Keep these files together as one evidence set:

- original planning roster
- exact split manifest bytes
- exact analysis plan bytes
- sealed evidence JSONL
- every observation artifact named by the rows
- every clock-mapping artifact named by the rows
- truth-instrument records needed to audit the declarations
- canonical score report
- operator log and consent records, stored under their proper access controls

The report records:

- schema and physical-evidence limitation;
- gate, round, manifest, plan, detector, representation, and transform binding;
- exact JSONL, canonical row-set, and per-split row-set hashes;
- observation-artifact hashes;
- frozen reference-model identity for Gate 1 and Gate 2;
- cohort, participant, and remount summaries;
- counts, rates, latency, class metrics, and confusion matrix;
- every point, confidence boundary, method, and criterion outcome; and
- final disposition.

The report is deterministic for the same exact inputs. The exact JSONL hash
and order-independent row-set hash serve different purposes and should both be
retained. Do not copy metrics into a hand-edited summary and discard the
machine-readable report.

## Failure recovery

Fail closed. The current schema has no compatibility or missing-row path.

- Before the artifacts are frozen, correct the roster and regenerate both
  manifest and plan.
- After freezing but before the first prompt, discard the unused round and
  issue a new round ID if any frozen fact changes.
- If equipment fails before a prompt or truth interval begins, repair it and
  perform preflight again. No trial has occurred.
- Once a prompt is presented or a truth interval begins, never retry or replace
  that row based on its outcome.
- If the interval completed honestly, seal misses, false activations,
  duplicates, and poor latency as observed.
- If an interval cannot produce an honest complete row, stop the round,
  preserve the partial artifacts as failed evidence, and start a new round with
  a new roster and plan.
- A missing roster row, extra row, altered roster identity, duplicate prompt,
  duplicate row hash, duplicate event ID, semantic row tamper, or reformatted
  bound artifact is a hard scoring error.
- If transfer corruption occurs, restore only the exact bytes from the
  authoritative immutable copy. Do not reconstruct or reseal measurements.
- Unsupported schemas are not converted. Capture a new current-schema round.

If consent is withdrawn, honor the withdrawal and treat any now-incomplete
frozen round as unusable. Exact roster equality is subordinate to participant
rights, not a reason to retain data.

## Privacy, consent, and honest capture

Use pseudonymous participant IDs in the evidence. Keep the identity key and
signed consent outside the study artifact with stricter access control. Do not
put names, email addresses, or other direct identifiers into IDs or prompt
fields.

Consent must cover:

- wearable motion sensing;
- the exact independent truth source;
- video or keyboard logging when used;
- retention duration and access;
- whether de-identified evidence will be shared; and
- withdrawal handling, including the possibility that withdrawal invalidates
  a complete frozen round.

The software prevents candidate inference from being declared as independent
truth, but it cannot authenticate a contact switch, camera, keyboard, or
tracker. Keep truth capture independent of candidate output. Do not let the
operator trim intervals, choose labels, repeat trials, or stop collection after
viewing candidate performance.

## Physical-only limitations

No software-only test or score can establish:

- that the declared participant performed the prompt;
- that participant IDs do not hide the same person across splits;
- that the rig, controller, surface, hand, or mount matches its declared ID;
- that a second mount ID represents a real removal and remount;
- that the truth source was attached, calibrated, synchronized, or read
  correctly;
- that a clock-mapping artifact is physically accurate;
- that an observation artifact came from the declared sensor or interval;
- that a reserved-position contact occurred at the intended location;
- that the detector and transform hashes identify the binaries actually used
  during capture;
- that prior participant practice did not change behavior; or
- that results generalize beyond the tested participants, surfaces, mounts,
  speeds, postures, and finite observation time.

Those facts need a physical prototype, controlled execution, independent
instrumentation, and an auditable operator record. A `go` report is evidence
for the declared finite study, not a production-readiness certificate.

## Execution checklist

- [ ] Gate 0A evidence for the exact rig is complete.
- [ ] Contact detector and any transform are frozen and hashed.
- [ ] Independent truth and clock mapping are ready.
- [ ] Consent and pseudonymous IDs are complete.
- [ ] Real participants are assigned to one split each.
- [ ] Every session, mount, artifact, and participant is split-owned.
- [ ] Heldout participant, remount, hand, class, and negative-exposure coverage
      meets the gate table.
- [ ] The roster has contiguous prompt ordinals from zero per session.
- [ ] Manifest and canonical v1 plan are frozen and archived before prompting.
- [ ] Every prompt is presented once.
- [ ] Every completed interval is sealed immediately, including bad outcomes.
- [ ] The final JSONL exactly matches the frozen roster.
- [ ] Scoring uses the exact manifest and plan bytes bound by the rows.
- [ ] The canonical report and all named source artifacts are retained.
- [ ] The disposition is reported as `go`, `inconclusive`, or `no-go` without
      reinterpretation.
- [ ] Physical-only limitations accompany every published result.
