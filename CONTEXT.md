# Invisible keyboard domain context

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

