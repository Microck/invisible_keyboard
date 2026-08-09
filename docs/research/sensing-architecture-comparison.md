# Sensing architecture comparison for an invisible keyboard

Access date: 2026-08-09

## Result

No sensing family is a universal winner. The best first experiment depends on
what "invisible" is allowed to include. The current evidence supports two
different leading prototypes:

1. A TapType-style pair of high-rate wrist inertial bands is the cheapest
   decisive test for ten-finger typing on a passive hard surface. This is only
   the lead if personal calibration and an English language model are allowed.
   The sensors identify taps and likely fingers, not physical key positions.
2. Egocentric optical hand tracking is the lead when physical key geometry,
   higher demonstrated live speed, and a headset or camera are acceptable.
   StegoType ran live on a Quest 3, but cameras add occlusion, lighting,
   compute, and privacy costs.

Radar and magnetic tracking remain credible fallbacks. Radar has a strong
surface-key demonstration, but mmKey used a custom 60 GHz platform that is not
available. Magnetic tracking has excellent laboratory position evidence, but
multi-finger hardware is bulky. Acoustic, force, capacitive, and strain sensors
are useful supporting channels but lack decisive continuous ten-finger text
evidence on a generic surface. Surface EMG is a long-term research lane, not a
cheap first prototype.

This is a conditional ranking, not the human-only architecture decision.

## What must be observed

Typing is not one sensing problem. A usable system needs evidence for all of
these stages:

1. **Intent or contact:** did the user mean to select something?
2. **Finger identity:** which finger acted?
3. **Layout-relative position:** where was the acting finger relative to the
   registered keyboard layout?
4. **Symbol decoding:** which key or character follows from that evidence?
5. **Continuous operation:** does it still work across corrections, pauses,
   hand shifts, rollover, ordinary non-typing motion, and new sessions?

A tap detector is not a keyboard. A classifier can report high accuracy on
isolated gestures while providing no evidence about false activations or text
entry. A language model can repair ambiguous evidence, but the result must be
reported separately from sensor-only decoding.

In this report, **direct key observability** means the sensor supplies a
layout-relative contact or position measurement that can distinguish nearby
keys without deriving the distinction only from a language prior. Optical,
radar, acoustic localization, magnetic tracking, and an instrumented surface
can do this in principle. Wrist IMU and EMG systems do not directly observe it.

## Evidence grades

- **I:** peer-reviewed primary paper or official proceedings copy read in full
- **II:** primary preprint, author manuscript, institutional record, dataset,
  or reproducible repository
- **III:** official current vendor documentation
- **IV:** secondary index or synthesis, used only for discovery or context
- **V:** repository issue or project status evidence
- **VI:** community anecdote; useful for failure discovery, not performance

Unknown fields remain unknown. Metrics from different tasks are not compared
as if they were interchangeable.

## Conditional shortlist

| Rank | Option | Exact evidence basis | Condition that makes it lead | Blocking uncertainty |
| --- | --- | --- | --- | --- |
| 1A | Two wrist bands with high-rate accelerometers | TapType: online N=10, mean 19.2 WPM and 0.6% CER after about 30 minutes; 1.6 kHz BMA456 sensing; 65 ms tethered or 135 ms BLE feedback; about 30 hours at 10-15 mW | Passive hard surface, two wristbands, personal 30-tap-per-finger calibration, and English n-gram decoding are acceptable | Same-finger keys are language-model choices, not measured positions; continuous false activations and cross-day remount transfer need a new test |
| 1B | Egocentric optical or IR hand tracking | StegoType: Quest 3, 30 Hz UmeTrack, online N=18, 42.4 WPM and 7% UER, mean model emission latency 27 ms, no language model; training corpus 268K transcriptions from 606 typists | A headset or camera and camera privacy are acceptable; direct hand geometry matters more than minimum BOM | Ring/pinky occlusion, surface registration, correction burden, and behavior shift from training collection to live use |
| 3 | Wrist IMU plus optical geometry | Complementary physics: IMU supplies high-rate impact/finger evidence while vision supplies layout-relative geometry; prior TapID work demonstrates the combination for rapid VR touch | A camera is already present and a second wearable channel is acceptable | No source reviewed here proves that this exact hybrid beats StegoType or TapType on live text; it must earn its complexity in an ablation |
| 4 | 60 GHz radar surface localization | mmKey: N=10, over 95% single-key, over 90% multi-key, over 97% word recognition after commercial correction, three calibration presses, and operation to 45 cm | No wearable and no RGB camera are high priorities; printed or registered layout and radar hardware are acceptable | mmKey's custom Qualcomm array is unavailable; no evidence shows a current TI EVM reproduces its super-resolution or live WPM |
| 5 | Magnetic multi-finger tracking | Finexus: five fingertips, 0.95 mm fixed-orientation and 1.33 mm random-orientation accuracy within 120 mm, 320 Hz and 2.22 ms compute; AuraRing shows a lower-power one-ring path | A bulky lab rig is acceptable to establish a geometric upper bound | Finexus requires fingertip emitters, external coils/electronics, and wires; neither project demonstrates continuous keyboardless text |
| 6 | Bilateral sEMG | emg2qwerty: 108 users, 346 hours, 32 channels at 2 kHz; personalized eight-user mean CER about 6.95% with LM | Long personal training, research electrodes, and biometric-signal handling are acceptable | Data were captured while typing on a physical keyboard; generic unseen-user mean CER is about 51.8%; research hardware is unavailable and the repository is archived |

Ranks 1A and 1B are deliberately tied to different product definitions. The
human decision about surface, camera, headset, language dependence, and
wearables must break the tie.

## Option matrix

### Observability and demonstrated task

| Sensing family | Tap or intent evidence | Physical key identity | Best continuous text evidence found | Required body or environment |
| --- | --- | --- | --- | --- |
| Wrist IMU | Impact microvibration is inferred with high temporal detail | No. TapType estimates finger probabilities; same-finger keys remain ambiguous | TapType N=10 online: 19.2 WPM, 0.6% CER; language-model-dependent | Two wristbands and a rigid, flat surface |
| Finger or ring IMU | Touch-down/up and ring orientation | No absolute key coordinate; QwertyRing maps index-finger orientation through a trigram model | QwertyRing five-day study N=16: 13.74 WPM initially, 20.59 WPM at end; top-1 decoder 79.0%, top-5 94.6% | One index-finger ring, resting wrist, desk-like surface, external text feedback |
| Contact, force, strain, piezo | Direct contact/force on an instrumented surface; strain observes bend; piezo observes dynamic impact | Only an electrode/sensor array provides direct key location. A single wearable or piezo channel does not | No verified generic-surface, ten-finger continuous QWERTY result in this pass | Instrumented surface, glove, finger pads, or attached transducers depending on design |
| Capacitive or electric field | Direct coupling, proximity, or touch state | Possible on an electrode array or tracked skin region; not on an arbitrary uninstrumented desk from one ring | ElectroRing/SkinTrack/EFRing show touch or pose primitives, not decisive continuous QWERTY | Ring/wrist electrodes, skin or surface coupling, and stable grounding geometry |
| RGB, IR, depth, hand tracking | Contact is inferred from pose or image evidence | Yes, after registering a surface and layout; motion models may decode keys end to end | StegoType N=18 online: 42.4 WPM, 7% UER on Quest 3; 2020 OptiTrack study N=20 offline: 73 WPM, 2.38% UER | Headset or external cameras with view of both hands and surface |
| mmWave radar | Motion and impact signatures | mmKey localized registered surface keys with custom antenna aperture | mmKey key/word recognition only; no verified live WPM or UER | 60 GHz radio facing surface, registered or printed layout, clear sensing volume |
| Acoustic or ultrasonic | Strong contact transient; time-difference or active sonar can localize | Possible on calibrated hard surfaces; material and geometry affect propagation | Acustico and UbiK demonstrate localization primitives; no verified continuous ten-finger text metric in this pass | Microphones or transducers, known surface geometry, manageable ambient noise |
| Magnetic | Relative 3D/5-DoF position is direct; contact still inferred or fused | Yes within calibrated field and layout | Millimeter-scale tracking, but no continuous text study | Finger/ring emitters and wrist or environmental receiver coils |
| sEMG | Muscle activation precedes visible motion and carries finger-intent evidence | No absolute key position | emg2qwerty continuous offline decoding, but all labels came from a physical keyboard | Bilateral multi-channel forearm electrodes with repeatable placement |
| Hybrid | Uses independent channels for contact, finger, and geometry | Depends on geometry channel | TapID supports the concept; no reviewed hybrid establishes the final product target | Sum of both systems' wearables, compute, calibration, and privacy burden |

### Engineering characteristics

| Family | Rate, latency, power evidence | Calibration and transfer | Main failures | Privacy | Availability and BOM | Cheap decisive prototype? |
| --- | --- | --- | --- | --- | --- | --- |
| Wrist IMU | TapType: 1.6 kHz; 59 ms contact-to-event tethered, 129 ms BLE; 6.5 ms inference; 10-15 mW, about 30 h coin cell | 30 taps per finger; cross-block and cross-participant fine-tuning reported, but cross-day remount evidence is not decisive | Same-finger ambiguity, hand shift, weak/soft taps, ordinary impacts, surface transfer, BLE delay | Low environment capture; high typed-text sensitivity | Low component tier, but custom bands/firmware | **Yes**, for a surface + LM product definition |
| Finger/ring IMU | QwertyRing sampled 1 kHz and reports about 14 ms touch-down delay; authors found 200 Hz sufficient; battery life unknown | Personal distribution updates; orientation changes with ring fit and wrist placement | More than 1 cm position error, row ambiguity, OOV failure, single-finger throughput ceiling | Low environment capture | Low component tier; custom ring | Yes for one-finger text, not decisive for ten fingers |
| Force/strain/piezo | Usually low-to-moderate bandwidth and power, but comparable system figures were not verified | Surface mounting, preload, hysteresis, glove fit, and user anatomy | Drift, creep, hysteresis, missed light taps; piezo cannot measure a held static state | Low | Low parts cost; mechanical integration dominates | Only as a tap-label or chord experiment |
| Capacitive/EF | Exact comparable rate/power unknown; can be low power | Strong dependence on skin contact, ring fit, grounding, humidity, and session | Coupling drift, nearby conductive objects, user transfer, no world coordinate from one channel | Low-to-medium; captures body coupling/pose | Low-to-medium component tier; custom analog front end | No for full QWERTY alone; yes for contact gating |
| Optical/IR/depth | StegoType: 30 Hz frames, 27 ms mean model emission, on-device Quest 3; isolated tracker power unknown | Camera/surface extrinsics plus hand model; StegoType trained across 606 people and tested 11 unseen offline users before live study | Self-occlusion, ring/pinky loss, sunlight/reflective surfaces, view loss, surface registration, compute | High: hands, room, and bystanders may be imaged | Medium if tracker/headset already owned; otherwise camera plus host compute | **Yes**, if camera/headset is in scope |
| Radar | mmKey live pipeline rate/power and raw bandwidth not reported here; raw ADC capture is data-heavy | Three key presses, but radio/keyboard movement requires recalibration | Overlapping motion above about 120 keystrokes/min, clutter, aperture/resolution limit, layout movement | Medium: no RGB image, but motion/occupancy and inferred text remain sensitive | Medium-to-high; current TI EVM exists, mmKey custom array does not | No until a current board passes adjacent-key localization |
| Acoustic/ultrasonic | Comparable latency/power unknown; audio-rate sampling is expected but not used as a verified claim | Surface material, microphone spacing, orientation, and layout registration | Ambient sound, soft/irregular surfaces, echoes, hand damping, multiple simultaneous taps | Medium-to-high if microphones retain ambient audio | Low-to-medium; commodity microphones/transducers | Yes as a cheap localization/fusion probe, not as proven text entry |
| Magnetic | Finexus: 320 Hz, 2.22 ms compute; AuraRing: 2.3 mW ring, about one day | Factory calibration can transfer for AuraRing; nearby metal and receiver/keyboard movement matter | Short 10-15 cm range, field distortion, bulky multipoint emitters, distal-joint ambiguity for ring placement | Low environment capture | Medium custom hardware; no ready five-finger product | Yes only as a lab geometry upper bound |
| sEMG | 32 channels at 2 kHz in emg2qwerty; power/online latency not established | Electrode placement, skin, fatigue, day, and user dominate; personal fine-tuning is essential | Generic transfer, don/doff shift, sweat, gesture crosstalk, unavailable research hardware | Very high: biometric and inferred-intent signal | High/research-only; Meta hardware unavailable, Myo lower-rate/discontinued | No |
| Hybrid | Sum of streams; synchronization becomes a first-order requirement | Must calibrate channels and their coordinate/time relation | Complexity can mask which channel helps; one failed channel can poison fusion | Union of both privacy risks | Medium-to-high | Only after a single channel fails a named gate |

## Modality findings

### 1. Finger and wrist inertial sensing

TapType is the strongest cheap passive-surface evidence. Its wristbands use
high-rate BMA456 accelerometers to classify finger probabilities from impact
microvibrations. A Bayesian decoder combines those probabilities with the
known touch-typing finger-to-key map and an n-gram language model. It therefore
does **not** physically observe whether, for example, a left index finger meant
R, F, V, T, G, or B. The reported 19.2 WPM and 0.6% CER are real online study
metrics for ten participants, but they are conditional on language, phrase
distribution, personal 30-tap-per-finger fine-tuning, and about 30 minutes of
practice. Sensor-only, no-language-model, OOV, cross-day, and continuous
false-activation results must be separate prototype outputs.

QwertyRing proves a different point. A six-axis IMU ring can detect contact and
use index-finger orientation plus a trigram model for one-finger surface text.
Its five-day study reaches 20.59 WPM, but it cannot enter out-of-vocabulary
words and its raw position error exceeds 1 cm even after calibration. It is a
good reduced-input option, not evidence that a ring recovers ten-finger QWERTY
key identity.

**Air typing:** IMUs still observe motion, but without a surface impact or an
external position reference they lose the strongest segmentation cue and do
not establish layout-relative key position. No reviewed IMU work justifies
full ten-finger air typing as the cheap first lane.

### 2. Contact, force, strain, bend, and piezoelectric sensing

These sensors answer contact or deformation well. They answer key identity
only when spatially distributed across a surface or when paired with a
position channel. TOAST is relevant as an instrumented touch-sensitive
surface; ForceBoard demonstrates force-based text interaction with a reduced
one-dimensional alphabet. Soft bend and piezoresistive gloves demonstrate
finger-shape sensing. None is primary evidence that one cheap wearable can
decode continuous ten-finger QWERTY on any desk.

Piezo sensors are especially useful for sharp tap timestamps and weak for
static contact. Strain and bend sensors add hysteresis, fit, and anatomical
transfer. A full surface array works but changes the product from an invisible
keyboard into a hidden physical keyboard. The decisive use is ground truth or
a secondary contact gate, not the first standalone decoder.

### 3. Capacitive and electric-field sensing

ElectroRing, EFRing, SkinTrack, Z-Pose, and Ring-a-Pose show that body-coupled
electric fields or impedance can detect touch, track a contact on skin, or
infer hand pose. The evidence also shows the central transfer problem: coupling
changes across fit, people, sessions, skin state, and grounding. One ring does
not provide a stable world coordinate on an arbitrary desk. A surface electrode
array can provide that coordinate, but again instruments the keyboard area.

This lane is promising as a low-power tap/intent gate in a hybrid. It is not a
decisive full-QWERTY prototype on its own.

### 4. Cameras, IR, depth, and skeletal hand tracking

This family best observes the geometry that inertial and EMG systems lack.
The 2020 hand-tracking study used a near-perfect multi-camera OptiTrack setup
and marker gloves. Its 20-person, 73 WPM, 2.38% UER result was offline, so it
is an upper bound, not a commodity-camera product metric.

StegoType closes much of that gap. It uses a stereo pair of downward-facing
cameras, UmeTrack at 30 Hz, and a streaming model on a Quest 3. The training
set contains 268K transcriptions from 606 typists. Its live N=18 comparison
reached 42.4 WPM and 7% UER versus 74.5 WPM and 0.8% UER on the physical
keyboard. It used no language model. Its paper also documents self-occlusion,
ring/pinky tracking failures, slow correction, and a behavior gap: training
participants could not rest fingers on the pressure-label surface, while live
users did.

The modality can support air or surface typing. Surface contact reduces
fatigue and geometric ambiguity, while air typing must infer a virtual plane
and selection event and generally loses haptic anchoring. A commodity optical
prototype is decisive if a headset/camera is allowed, but it must measure
lighting, occlusion, bystander capture, and end-to-end rather than model-only
latency.

### 5. mmWave and radar

mmKey contradicts the broad claim that radar cannot distinguish adjacent keys.
Its custom 60 GHz 802.11ad/ay array did so after three calibration presses,
with over 95% single-key and over 90% multi-key accuracy for ten people. Word
recognition exceeded 97% only after commercial text correction. It did not
report a comparable live WPM/UER study, and performance degraded above about
120 keystrokes per minute as motion overlapped.

The result does not transfer automatically to a current radar dev board. The
paper used a customized Qualcomm platform with added antenna aperture, called
the prototype bulky, and said the testbed was not publicly available. TI now
sells the 57-64 GHz IWRL6432BOOST with 3 RX and 2 TX; raw ADC capture requires
the separate DCA1000EVM. No source reviewed here shows that this topology
reproduces mmKey's resolution. The first radar step is therefore an adjacent-
key localization gate, not text-model training.

### 6. Acoustic and ultrasonic sensing

Acustico uses wrist-worn microphones and the timing difference between
surface-borne and airborne sound to localize taps on uninstrumented surfaces.
UbiK uses acoustic fingerprints for a printed surface keyboard. Active sonar
can track a finger in a bounded area. These are good physical matches for
cheap tap timing and coarse 2D localization, but this pass did not verify a
continuous ten-finger WPM/UER result comparable to TapType or StegoType.

Surface material, echoes, microphone geometry, hand damping, ambient noise,
and simultaneous taps are likely decisive. Microphones also create an ambient
audio privacy risk unless capture and retention are tightly bounded.

### 7. Magnetic sensing

Finexus demonstrates excellent five-fingertip position sensing: 0.95 mm mean
error with fixed orientation and 1.33 mm with random orientation inside its
120 mm volume, at 320 Hz. That is enough geometric resolution for key identity
in a lab. The system uses frequency-multiplexed electromagnets on fingertips,
four magnetometers, an amplifier, and wired laboratory electronics; its
93,397-point evaluation was a tracker benchmark, not typing.

AuraRing shows a more wearable one-finger path: 5-DoF tracking, 4.4 mm mean
dynamic accuracy, 2.3 mW at the ring, and about one day of battery. Its range is
only 10-15 cm, nearby metal distorts the field, and a ring on a proximal
phalanx does not fully observe distal motion. Magnetic sensing is a useful
geometry upper bound or hybrid position channel, not a ready ten-finger product.

### 8. Surface EMG

emg2qwerty is the largest evidence base reviewed: 108 users, 1,135 sessions,
346 hours, bilateral 32-channel 2 kHz sEMG, and continuous text decoding. It
also exposes why this is not the first prototype. Participants typed on a
physical Apple Magic Keyboard, so the study does not prove keyboardless use.
Across eight held-out test users, the published repository scores average about
51.8% CER for a generic model with language modeling and about 6.95% after
personal fine-tuning. The project repository is archived, its research hardware
is unavailable, and its license is noncommercial. Issue 65 notes that each
personalized subject result was not averaged over multiple seeds.

An independent two-day, 19-person isolated-key study reported 87.4% +/- 2.5%
within-session accuracy and materially worse inter-session and inter-person
transfer. That supports, rather than resolves, the calibration concern. sEMG
directly observes neuromuscular intent but not layout-relative position, and it
creates the most sensitive biometric-data obligations in the matrix.

## Defensible hybrids

Hybrids should combine missing physical observables, not simply add sensors.

1. **Wrist IMU + optical geometry:** high-rate impact and likely-finger evidence
   plus layout-relative hand position. This is the strongest fallback for XR.
   Require an ablation showing lower UER or false activations than optical alone.
2. **Wrist IMU + acoustic localization:** both are cheap and surface-based; IMU
   helps finger/tap evidence while acoustic timing adds coarse position. It is
   a research hypothesis, not an established continuous-text result.
3. **IMU + force/piezo ground truth:** useful for building labels and measuring
   missed taps. Remove the instrumented surface at evaluation if the product is
   supposed to work on any desk.
4. **Magnetic geometry + IMU contact:** direct relative position plus crisp
   impact timing. Useful as a lab reference even if mechanically unsuitable.
5. **Do not start with IMU + sEMG:** it compounds wearables, synchronization,
   personal calibration, and signal drift before either lane proves the basic
   product definition.

## Decisive prototype evidence contract

The LapCal audit changes how every option must be tested. A model result is
inadmissible until the acquisition topology satisfies these prerequisites.
These headings are evidence requirements, not aliases for the canonical
feasibility gates.

### Acquisition prerequisite: full-topology stream and synchronization

- Run every sensor, radio, host receiver, and label source in the intended
  topology, not one board in isolation.
- Give every packet a session ID, source ID, monotonic timestamp, and sequence
  number. Labels and sensor frames must share one monotonic timeline. If a
  device clock cannot be shared, retain both raw clocks plus measured sync
  events and the clock-conversion fit.
- Measure achieved rate, jitter, packet loss, disconnects, clock offset, and
  clock drift for every source. Do not infer rate from firmware settings.
- Do not begin label collection or modeling until the full topology streams
  concurrently and the synchronization error is bounded for the target event.

### Immutable-evidence prerequisite: complete sessions

- Write raw sensor packets, label events, layout registration, firmware and
  model versions, device placement, participant, surface/environment, and
  clock metadata into a session manifest.
- Close sessions immutably and hash the raw files. Cleaning, alignment, and
  windowing create derived artifacts; they never rewrite raw input.
- Record negative sessions containing ordinary desk work, gestures, walking,
  drinking, and hand rests. False activations per hour require continuous
  negative time, not balanced gesture clips.

### Evaluation prerequisite: leakage-resistant decoding

- Assign whole sessions to train, validation, and test before windowing,
  overlap, feature extraction, augmentation, or personalization.
- Report session-held-out results first. Add user-held-out and cross-day/remount
  results where the product claim needs them.
- Report tap detection, finger identity, physical key evidence, sensor-only
  decoding, language-model decoding, and correction as separate stages.
- Keep the test set and negative sessions untouched while tuning.

### Product-measurement prerequisite: live text and complete-system evidence

- Report online WPM, CER/UER, corrected error, backspaces, substitutions,
  misses, false activations per hour, and p50/p95 end-to-end key latency.
- Include OOV words, names, numbers, punctuation, shortcuts, rollover, pauses,
  and movement of hands relative to the initial layout.
- Repeat after don/doff, a new day, different surfaces or lighting, and the
  intended posture. Report setup and recalibration time.
- Measure stream power and battery life in the same topology used for the live
  result.

Ticket 005 should set numeric pass/fail thresholds before prototype data are
collected. This report intentionally does not invent them.

## Claim audit

| Initial or common claim | Audit result | Disposition |
| --- | --- | --- |
| "A strong tap detector is most of the keyboard" | Contradicted | Tap timing does not supply key identity, language-independent symbol decoding, or continuous false-activation evidence |
| "Wrist IMUs identify keys" | Downgraded | TapType estimates finger probabilities. Its n-gram decoder selects among the keys assigned to that finger |
| "TapType proves language-independent text" | Contradicted | Its headline online result is conditional on Bayesian decoding with an n-gram language model and phrase-set vocabulary |
| "The 73 WPM vision result is live commodity hand tracking" | Contradicted | The 2020 result is offline and uses multi-camera OptiTrack plus marker gloves; StegoType is the relevant live commodity-headset result at 42.4 WPM and 7% UER |
| "Radar cannot resolve adjacent keys" | Contradicted, then bounded | mmKey resolves them on custom high-aperture 60 GHz hardware; reproduction on a current commodity EVM remains unproved |
| "mmKey is a cheap current prototype recipe" | Contradicted | Its customized Qualcomm platform was bulky and unavailable; no current board equivalence was established |
| "Magnetic sensing is too inaccurate" | Contradicted | Finexus is millimeter-scale; the blocking problem is multi-finger form factor and operating volume, not lab accuracy |
| "sEMG already works without a keyboard" | Contradicted | emg2qwerty labels came from physical-keyboard typing; generic transfer remains poor |
| "High isolated classification accuracy predicts text entry" | Contradicted | sEMG and many contact/gesture papers lack live sequence, correction, false-activation, and user-transfer evidence |
| "Piezo, force, or capacitance directly gives key identity" | Downgraded | Only a spatial array or separately tracked coordinate does so; one wearable channel mainly supplies contact/deformation |
| "Combining sensors must improve accuracy" | Unproved | Fusion is justified only when channels observe complementary variables and an ablation earns the added burden |
| "The LapCal model is a usable baseline" | Contradicted by local audit | Its data split leaked overlapping windows and the full six-sensor BLE/synchronization path was not established |

## Abandoned and availability signals

- The local LapCal project contains useful hardware artifacts but no valid live
  key-identity result; it is not an implementation base.
- The emg2qwerty GitHub repository is archived, research hardware is not sold,
  and the code/data license is noncommercial.
- mmKey's authors described the prototype as bulky and expected a Qualcomm
  testbed release that is not available in the reviewed material.
- Tap Strap community reports describe hard-surface dependence, range blockage,
  and miskeys. These are anecdotes and Tap Strap is a learned chord system, not
  QWERTY muscle-memory evidence.
- An older Leap Motion community thread reports occlusion, bright-light and
  reflective-surface failures, and CPU load. It is an obsolete hardware
  generation, so it informs test cases rather than current performance.

## Evidence registry

All sources were accessed on 2026-08-09.

| ID | Grade | Source | Claim used and boundary |
| --- | --- | --- | --- |
| S1 | I | [TapType author/CHI manuscript](https://arxiv.org/html/2410.06001) | Hardware, 1.6 kHz, power, latency, N=10 online text, calibration, decoder structure, 19.2 WPM/0.6% CER |
| S2 | I | [TapType DOI](https://doi.org/10.1145/3491102.3501878) | Stable peer-reviewed record |
| S3 | I | [QwertyRing DOI](https://doi.org/10.1145/3432204) | Peer-reviewed one-ring surface-text study |
| S4 | I | [QwertyRing author PDF](https://pi.cs.tsinghua.edu.cn/lab/papers/QwertyRing.pdf) | N=12/12/16 studies, 1 kHz sensing, touch latency, decoder and five-day text metrics, OOV/position limitations |
| S5 | I | [StegoType DOI](https://doi.org/10.1145/3654777.3676343) | Peer-reviewed live egocentric surface typing |
| S6 | I | [StegoType author PDF](https://www.keithv.com/pub/stegotype/richardson_stegotype.pdf) | Quest 3/UmeTrack setup, 606-person training corpus, 30 Hz, 27 ms model emission, online N=18, 42.4 WPM/7% UER, limitations |
| S7 | I | [Decoding Surface Touch Typing DOI](https://doi.org/10.1145/3379337.3415816) | Peer-reviewed OptiTrack upper bound |
| S8 | II | [MIT institutional record](https://hdl.handle.net/1721.1/158198) | N=20 and offline 73 WPM/2.38% UER boundary |
| S9 | I | [mmKey DOI](https://doi.org/10.1109/JIOT.2021.3084560) | Peer-reviewed 60 GHz surface-key system |
| S10 | I | [mmKey author PDF](https://www.originwirelessai.com/wp-content/uploads/2024/04/mmKey_Yuqian_2021-min.pdf) | N=10 key metrics, correction boundary, calibration, speed/distance limits, unavailable custom platform |
| S11 | III | [TI IWRL6432BOOST](https://www.ti.com/tool/IWRL6432BOOST) | Current 57-64 GHz 3-RX/2-TX EVM and separate raw-capture requirement; not evidence of mmKey equivalence |
| S12 | I | [Acustico DOI](https://doi.org/10.1145/3379337.3415901) | Passive wrist acoustic 2D tap localization; no comparable continuous-text metric extracted |
| S13 | I | [UbiK DOI](https://doi.org/10.1145/2594368.2594384) | Acoustic surface-keyboard precedent; exact current product metrics unknown |
| S14 | I | [AuraRing DOI](https://doi.org/10.1145/3369831) | Peer-reviewed one-ring magnetic 5-DoF tracking |
| S15 | I | [AuraRing author PDF](https://www.ericwhitmire.com/pdfs/auraring.pdf) | 4.4 mm dynamic accuracy, optional 1.5 mm calibration, 2.3 mW, range and metal limits |
| S16 | I | [Finexus DOI](https://doi.org/10.1145/2858036.2858125) | Peer-reviewed multipoint magnetic tracker |
| S17 | I | [Finexus author PDF](https://keyuc.com/research/Finexus/Finexus_CHI2016.pdf) | Five-fingertip accuracy, 320 Hz, latency/compute and bulky lab topology; not a text study |
| S18 | II | [emg2qwerty preprint](https://arxiv.org/html/2410.20081) | Dataset, architecture and keyboard-ground-truth boundary |
| S19 | I | [emg2qwerty NeurIPS paper](https://proceedings.neurips.cc/paper_files/paper/2024/file/a64d53074d011e49af1dfc72c332fe4b-Paper-Datasets_and_Benchmarks_Track.pdf) | Peer-reviewed 108-user/346-hour sEMG benchmark |
| S20 | II/V | [emg2qwerty repository](https://github.com/facebookresearch/emg2qwerty) | Exact per-user CER list, archived state, noncommercial license and unavailable hardware context |
| S21 | V | [emg2qwerty issue 65](https://github.com/facebookresearch/emg2qwerty/issues/65) | Personalized subject results were not averaged across seeds; unresolved statistical questions |
| S22 | I | [Two-day sEMG keypress study](https://pmc.ncbi.nlm.nih.gov/articles/PMC11909141/) | N=19, two sessions, isolated keys, 87.4% +/- 2.5% within-session result and poor transfer boundary |
| S23 | I | [EFRing DOI](https://doi.org/10.1145/3569478) | Electric-field ring pose primitive; not continuous full-QWERTY evidence |
| S24 | II | [ElectroRing author PDF](https://hbenko.com/publications/2021/ElectroRing%20-%20CHI2021.pdf) | Ring-only touch/release signal primitive |
| S25 | II | [SkinTrack author PDF](https://chrisharrison.net/projects/skintrack/skintrack.pdf) | Body-coupled electric-field touch tracking on skin; requires ring plus wrist electrodes |
| S26 | II | [Ring-a-Pose preprint](https://arxiv.org/html/2404.12980v2) | Impedance hand-pose work and explicit cross-session/user inconsistency of prior Z-Pose |
| S27 | I | [Z-Pose DOI](https://doi.org/10.1145/3615592.3616851) | Bioimpedance hand-pose precedent |
| S28 | I | [TOAST DOI](https://doi.org/10.1145/3191765) | Touch-sensitive large-surface precedent; requires an instrumented surface |
| S29 | II | [ForceBoard author PDF](http://mingyuanz.com/pdfs/18-forceboard.pdf) | Force-based reduced text interaction; not generic-surface QWERTY |
| S30 | II | [Soft stretch-bend sensor paper](https://pmc.ncbi.nlm.nih.gov/articles/PMC5133288/) | Wearable bend-sensing primitive; not continuous text evidence |
| S31 | I | [Piezoresistive data-glove paper](https://www.mdpi.com/2072-666X/13/3/372) | Glove pose/deformation sensing and its mechanical/calibration class |
| S32 | VI | [Tap Strap VR community review](https://www.reddit.com/r/Vive/comments/92nr2g/tap_strap_final_review_for_vr_nope/) | Anecdotal hard-surface, obstruction/range, and miskey test cases; not performance proof |
| S33 | VI | [Leap Motion community failure thread](https://www.reddit.com/r/oculus/comments/41me01/) | Anecdotal occlusion/light/reflective-surface/CPU test cases from an obsolete generation |
| S34 | III | [Leap Motion Controller 2](https://leap2.ultraleap.com/products/leap-motion-controller-2/) | Current optical component availability; vendor page does not prove typing metrics |
| S35 | Local audit | [LapCal evidence audit](lapcal-evidence-audit.md) | Monotonic acquisition, immutable-session, session-split, BLE/topology, and reproducibility requirements |

## Tool coverage and limitations

- Kagi authentication was checked and available. Kagi Assistant was the primary
  synthesis/triage lane; Kagi Search discovered sources; Kagi Extract and page
  analysis read primary papers and official documentation. Primary PDFs were
  also parsed locally where extraction was incomplete.
- GitHub repository inspection and issue review supplied the emg2qwerty
  availability/statistical pulse. Kagi Reddit searches supplied the required
  community pulse. Community posts only generated failure cases.
- **Perplexity Deep Research was unavailable: `403 Access forbidden: session
  token invalid or expired`.** It was not retried.
- **gpt2agent light Deep Research was unavailable: `required Turnstile
  challenge could not be solved`.** It was not retried.
- The 2020 optical PDF could not be retrieved directly in this environment;
  its N=20 and 73 WPM/2.38% UER boundary comes from the official MIT record and
  the hardware boundary from the later StegoType primary paper.
- Comparable battery, end-to-end latency, user/session split, and current
  component pricing were not reported for every modality. Matrix cells mark
  them unknown rather than estimating them.
- BOM tiers are relative engineering judgments, not vendor quotes. Ticket 007
  must obtain current procurement prices after the human narrows the product
  definition.
- This ticket compares architectures. It does not exhaust every commercial,
  patent, or abandoned-system lead; ticket 004 owns that prior-art inventory.

## Highest-impact implication for the next ticket

Ticket 004 should distinguish systems that preserve familiar QWERTY muscle
memory from chorded or reduced alphabets, and it should verify whether their
hardware, code, data, and licenses remain obtainable. Most importantly, it
should track **why** each prior system stopped: missing physical key identity,
language dependence, unavailable custom hardware, user/session transfer,
occlusion, correction burden, or product form factor. Those failure categories
now map directly to prototype gates instead of being a flat list of projects.
