# Invisible keyboard domain context

## Current project decision

The current product is a personal calibrated, wearable-only Core-28 keyboard
for passive unmarked surfaces. It uses five finger IMUs and one dorsal reference
IMU per hand, familiar split-QWERTY assignments, exact A-Z plus Space and
Backspace output, and no language model. A normal keyboard supplies temporary
setup labels; it is not required during invisible typing.

The physical manifest remains a 34-position grid. Core-28 enables A-Z, Space,
and Backspace. Semicolon, Comma, Period, Slash, left outer thumb, and right
outer thumb are reserved negative positions that emit nothing and preserve a
versioned future expansion path. Core-34 names only the possible future output
set after those reserved positions pass their own physical gates.

LapCal is a hard evidence reference for the 34-slot per-finger topology and
packed group/bit ordering. It does not supply metric key geometry or this
project's logical Core-28 map. The implementation is independently written
under a no-artifact-reuse policy; it does not claim a formal clean-room process.

Use the [reference architecture](docs/implementation/reference-architecture.md)
for the product contract and the
[first prototype roadmap](docs/implementation/first-prototype-roadmap.md) for
the build and execution order. The
[source integrity audit](docs/research/source-integrity-audit.md) records the
primary-source corrections behind the current acquisition contract. The
broader research dossier is an option-space snapshot, not the current
architecture selection.

The canonical build is Bazel. On Linux, `calibrate-hardware` owns two distinct
controller byte streams, one selected `EV_KEY` keyboard, the loopback setup
page, immutable evidence, model fitting, and held-out validation.
Keyboard identification and logical-layout confirmation are separate evidence;
capture cannot begin until the user accepts the frozen US-QWERTY physical-row
prompt. The host applies every manifest axis rotation exactly once after raw
frame identity binding. The browser's rendered-cue acknowledgment opens a
bounded response gate; it is not contact truth. A causal sensor-derived contact
observation must then identify exactly one expected hand and finger. No
contact, the wrong hand or finger, and multiple contacts are explicit rejected
observations. Keyboard warm start is a non-admission signal precheck. Disjoint
passive-surface fitting, threshold selection, and held-out evidence decide the
saved validation result. Physical separability remains unproved until the
specified rig is built and measured.

The persisted success state is `CalibrationPhase::ValidationPassed`, serialized
as `validation-passed`, with
`CalibrationSnapshot.heldout_validation_passed=true`. Failure is
`CalibrationPhase::CalibrationFailed`, serialized as `calibration-failed`.
`HeldOutValidation` authorizes the saved model and replay, not live output or
continuing wearable position; recomputation faults use
`HeldOutValidationError`. The system cannot detect wearable removal, movement,
or loosening. Each live process therefore needs a fresh
`WearSessionConfirmation` based on an explicit user attestation that both
wearables have not moved. Any source, watchdog, identity, clock, output, or
process fault terminally revokes that live instance. Each restart recomputes
the saved held-out evidence and requires a new confirmation; no path silently
rearms. If either wearable moved, the user must collect fresh held-out evidence
before confirming.

The planned Rev-A acquisition path configures each LSM6DS3TR-C for pulsed 208
Hz data-ready edges, reads direct motion registers, marks each completed cache
store on D9/P1.14, and selects the latest committed sample into a 200 Hz
aggregate under a later D8/P1.13 snapshot marker. Both markers use the same
cache lock and never overlap. Normal pulses are at least 2 us. Before sensor
I/O or interrupts, the controller emits the startup anchor `D8 high >=20 us,
both low >=20 us, D9 high >=20 us, both low >=20 us`. A successful D9 falling edge
is the cache-eligibility boundary; a later D8 rising edge is the aggregate
selection boundary. Nominally, 8 reads per second per sensor are intentionally
decimated. Selected samples retain their source-edge timestamps. Expected
decimation is not an acquisition overrun; unexpected pending-read, read,
freshness, marker, deadline, or transport faults are. Direct-register reads
cannot prove every internal conversion was retained without full pulsed-INT1
analyzer evidence.

Gate 0A scores the native little-endian `u16` analyzer capture directly. The
canonical DSLogic Plus trace has ten channels for six INT1 lines, D8, D9, SDA,
and SCL, sampled at 2 MHz for 7,220,000,000 words. Decoded NDJSON, external
evidence JSON, and human aggregates are not decisive inputs. Build,
finalization, hashing, and scoring run in a Crabbox; only the USB analyzer
capture runs on its compatible physical host.

The first one-hand rig is a planned falsification build. It can reject the
electrical topology or contact hypothesis after its physical gates run. It
cannot create a production Core-28 artifact. Exact calibration and live output
require two admitted hand rigs. No rig has been assembled and no physical gate
has passed.

## Concepts

### Invisible keyboard
The project target: a system that lets a person enter text without a visible or
physical array of keys. The phrase does not, by itself, require typing in free
air or prohibit a wearable or a generic surface.

### Keyless text entry
Text entry without discrete physical keys. This is the broader, less ambiguous
term for the product category.

### Air typing
A form of keyless text entry where typing gestures happen in free space. Air
typing excludes a required contact surface, but it may still use a wearable.

### Surface typing
A form of keyless text entry where a person taps a generic surface that has no
physical key array. The surface can provide contact evidence without defining
where keys are.

### Typing gesture
A deliberate finger action intended to produce a key event. A typing gesture
can include motion, contact, a chord, or a sequence. Do not call every typing
gesture a tap because some candidate systems do not require contact.

### Contact event
Evidence that a finger touched a surface. A contact event can help detect intent
or timing, but it is not yet a key event because the system may not know which
symbol the user intended.

### Key event
The semantic output consumed by an operating system or application, including
press, release, modifier, and repeat behavior. Sensor samples and model labels
are not key events.

### Sensing architecture
The chosen set of sensors, their location, and the signal path used to observe
typing gestures. A sensing architecture can be wearable, environmental, or a
hybrid.

### Decoder
The component that turns a time series of sensor observations into symbols or
key events. Intent detection, key identity, language correction, and operating
system injection are distinct decoder responsibilities even if one model later
implements several of them.

### False activation
A key event emitted when the user did not intend to type. False activations are
different from substitutions, where the user intended one symbol and the
decoder emitted another.

### Personal calibration
A bounded setup process that adapts the system to one user, session, hand, or
wearable fit. Calibration is not the same as collecting an unbounded personal
training corpus.

## Actions

### Calibrate
Estimate parameters needed to interpret a user's sensor stream before normal
text entry.

### Detect intent
Decide whether an observed movement represents an intended typing gesture.

### Decode a symbol
Choose the key or symbol intended by a detected typing gesture.

### Inject a key event
Deliver the decoded press, release, modifier, or repeat event to the target
operating system or application.

## Invariants

- A sensor reading is not a key event.
- Contact detection and key identity are separate facts.
- Offline classification accuracy alone does not prove usable text entry.
- A feasibility claim must state the user, session, layout, and evaluation
  boundary over which it was measured.
