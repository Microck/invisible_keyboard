# Prior art, products, and abandoned attempts

Access date: 2026-08-09

## Result

Finger-driven text entry without a visible or physical keyboard is feasible in
bounded conditions. It is not yet a proven general replacement for a physical
keyboard. The strongest evidence divides into two different products:

1. **Finger-sequence QWERTY.** TypeAnywhere ignores key coordinates. Two
   commercial Tap Strap devices report which finger touched a surface, and a
   language model reconstructs text from the QWERTY finger sequence. Its
   longitudinal study reached 70.6 WPM and 1.50% character error rate (CER) on
   a table after 2.5 hours across five days, and 43.9 WPM with 1.37% CER on a
   lap. Those results are impressive but do not prove arbitrary symbol entry:
   the sensor does not distinguish keys assigned to the same finger, and the
   decoder supplies that missing information from language context.
2. **Spatial QWERTY.** ATK, StegoType, QwertyRing, mmKey, and SelfieType try to
   recover an imaginary key position. The best live camera result reviewed is
   StegoType at 42.4 WPM and 7% uncorrected error rate (UER), N=18, on Quest 3.
   The strongest mid-air result is ATK at 29.2 WPM after four blocks, N=8, but
   it confirms words through a candidate interface and language model. These
   paths preserve more arbitrary-input potential, but pay for geometry with
   cameras, registration, occlusion, fatigue, or unavailable custom sensors.

There is no verified mass-market survivor that provides unrestricted,
long-form QWERTY at normal keyboard speed with no visible layout, no physical
keys, and no environmental constraint. Tap is the clearest surviving
keyboardless company, but its 2026 TapBand is a waitlisted smart-glasses
controller and its shop currently lists no Tap Strap or TapXR products. Its
status is an active product pivot, not confirmed closure. Twiddler 4 is the
useful counterexample: a tactile chorded keyboard remains in stock at US$229,
showing that a learned alphabet can survive commercially when it retains
reliable physical switches. It is not an invisible keyboard.

Only Gest has a directly verified company shutdown in this landscape. AirType
or Noki and Samsung SelfieType became publicly silent, but no source reviewed
announces their cancellation. LapCal is not archived and its remote was last
pushed in 2025, so the supplied description of it as abandoned is not a fact
that this report can confirm. Its technical evidence remains bounded by the
[separate audit](lapcal-evidence-audit.md).

## Status vocabulary

Status terms in this report are deliberately strict:

- **Shipped/current:** a first-party store, release, support update, or current
  product page demonstrates present availability or operation.
- **Shipped/legacy:** units shipped, but current new-stock or maintenance is not
  established.
- **Academic prototype:** a paper or demonstration exists, but no product claim
  is made.
- **Active repository prototype:** recent source or issue activity exists, but
  no product or complete evaluation exists.
- **Inactive or silent:** the last meaningful public activity is old. This does
  not prove cancellation.
- **Confirmed discontinued:** a first-party closure, cancellation, archive, or
  end-of-life statement exists.

Repository `updated_at` is not treated as meaningful activity because stars,
metadata, and automated operations can change it without new work. Push dates,
releases, issues, and first-party statements carry more weight.

## Interaction taxonomy

| Family | What the user intends | What must be sensed | Representative systems | Core trade |
| --- | --- | --- | --- | --- |
| Spatial QWERTY muscle memory | Press the remembered location of a normal key | Tap plus layout-relative position | ATK, QwertyRing, StegoType, mmKey, SelfieType | Familiar layout, but geometry and tactile orientation are hard |
| QWERTY finger sequence | Tap with the finger that normally owns the intended key | Tap time and finger identity; language supplies same-finger key identity | TypeAnywhere, TapType | Fast in predictable language; sensor alone cannot enter arbitrary same-finger symbols |
| Chorded alphabet | Produce a learned combination of fingers | Which fingers contact or move together | Tap Strap/TapXR, Chording Glove, KITTY | Robust event alphabet, but substantial learning and limited QWERTY transfer |
| Reduced fingertip or finger-surface layout | Select a group or region with the thumb | Relative thumb contact position and pressure | DigiTouch, TipText, BiTipText, PinchType | Tactile and portable, but lower throughput and a new mapping |
| Projected or marked surface | Press a visible or registered virtual key | Surface contact position | Canesta/Celluon class, Serafim Keybo, mmKey | Gives visual geometry, but is no longer truly invisible and still lacks key travel |
| Instrumented hidden surface | Press a remembered layout on a touch or force surface | Direct 2D contact | CHI 2018 "Typing on an Invisible Keyboard" | Strong touch evidence, but the surface is a keyboard sensor |
| Adjacent gesture controller | Point, click, or invoke commands rather than transcribe text | Hand pose or gestures | Mouseless, Ultraleap XR Keyboard | Reusable tracking primitive, not proof of long-form keyless typing |

## Dated landscape

Metrics below retain their task boundary. `Unknown` means no credible metric
was found, not zero performance.

| Year | System | Interaction and sensing | Public evidence and cohort | Status and availability on 2026-08-09 | Concrete constraint or stop signal |
| --- | --- | --- | --- | --- | --- |
| 1999 | Chording Glove | Learned chords through seven glove buttons | Academic study; 16.8 WPM after 10 hours is reported by later peer-reviewed work | Academic prototype; no current product found | Long training and a new alphabet |
| 2005 | KITTY | 48 thumb-to-finger contact combinations on a glove | Peer-reviewed VR text-entry prototype; no comparable live product metric recovered | Academic prototype | Dense contact map, glove burden, and learned combinations |
| 2000s-2010s | Canesta and Celluon projection keyboards | Visible laser QWERTY plus IR contact plane | ATK cites 37.7 WPM for Canesta; Celluon Epic and Serafim Keybo have manuals and legacy retail pages | Shipped/legacy category; current first-party new-stock was not established | Bright light, surface requirements, occlusion, no tactile key edges or travel |
| 2010 | Mouseless | Familiar mouse pose sensed by an IR laser plane and IR camera | Working US$20 prototype; UIST 2010 poster and CHI 2011 interactivity | First-party page is under `archived/projects` | It implements cursor motion and clicks, not text entry; it is a misleading keyboard precedent |
| 2014 | AirType, later Noki | Two hand cuffs; claimed ML inference from typing motion on a surface or in air | Concept coverage and a first-party social rebrand; no cohort, WPM, error rate, code, or shipped unit found | Silent, not confirmed discontinued | No published evidence that the cuffs could separate all keys or operate continuously |
| 2015 | ATK | Ten-finger QWERTY in air; Leap Motion at about 100 fps plus probabilistic tap/finger detection and word correction | N=8 expert typists; 23.0 WPM and 0.3% uncorrected word error in block 1, 29.2 WPM in block 4 | Academic prototype | Raw within-finger endpoint accuracy was 56.8-84.6%; correlated finger motion made five-letter finger-sequence accuracy about 56.8%; word candidates, confirmation gestures, visual feedback, and language repair the stream |
| 2015-2016 | Gest | Palm strap and finger bands with IMUs; typing was an experimental secondary mode | Kickstarter: 902 backers and US$199,988; no formal text-entry study | Confirmed discontinued on 2016-04-25; company said it would close and refund all backers | It could not secure the further funding required to guarantee production and delivery; English sentence typing was still experimental |
| 2017 | DigiTouch | Split QWERTY mapped to conductive-fabric finger strips; thumb position and pressure | N=10; ten 20-minute sessions; mean 7.0 to 16.0 WPM and 0.85% final UER | Academic prototype | Corrected-error burden remained about 15% in the last session; users struggled with dense regions, hand fit, and nonlinear fabric endpoints |
| 2017 | imutype bachelor prototype | IMU and ML virtual-keyboard glove | One-contributor thesis repository, one star, no release, no license, no reported live metric | Last push 2017; silent, not formally abandoned | Research artifact with no maintained runtime or replication record |
| 2018 | Typing on an Invisible Keyboard | Invisible layout on a large touch surface | Three-day study: 31.3 to 37.9 WPM after 20-25 minutes per day, versus 41.6 WPM visible | Academic prototype | Strong evidence for learned touch offsets, but the touchscreen directly measures every press; it does not remove the physical sensor surface |
| 2019 | TipText | Invisible 2x3 QWERTY-derived grid on the distal index finger; capacitive skin overlay | Performance study N=12; 11.9 WPM average, 13.3 WPM in final block, 0.30% UER | Academic prototype | Reduced six-key layout needs word disambiguation and a calibrated overlay; one hand limits throughput |
| 2020 | Samsung SelfieType | QWERTY muscle memory on a flat surface; phone selfie camera and proprietary AI | First-party CES concept description; no cohort, WPM, error rate, app, or product result | Publicly silent after January 2020; not confirmed discontinued | Front-camera field of view, device pose, occlusion, lighting, and no tactile landmarks; feasibility was never published |
| 2020 | PinchType | QWERTY finger groups selected by thumb-to-fingertip pinches; hand tracking plus language disambiguation | Preliminary N=14; 40 phrases; 12.54 WPM including correction and 20.07 WPM excluding correction time | Academic prototype | Correction time is large, same-finger letters remain language choices, and pinching is a different motor action from typing |
| 2020 | QwertyRing | One index-finger IMU ring, imagined surface QWERTY, trigram decoder | Studies N=12, N=12, and longitudinal N=16; 13.74 to 20.59 WPM over five days; top-1 79.0%, top-5 94.6% | Academic prototype | One-finger ceiling, greater than 1 cm raw position error, personal fit/orientation, and out-of-vocabulary limits |
| 2021 | mmKey | Registered or printed surface layout localized by custom 60 GHz radar | N=10; greater than 95% single-key, greater than 90% multi-key, greater than 97% word recognition after commercial correction; no live WPM | Academic prototype; custom Qualcomm array not available | Overlapping motion above about 120 keystrokes/min, clutter and aperture limits, surface registration, and unavailable reference hardware |
| 2021-2023 | Ultraleap XR Keyboard | Visible virtual keys pressed through optical hand tracking in Unity XR | Open Apache-2.0 repository, 56 stars, release 1.1.0 in 2023; intended for short strings | Repository explicitly says it is not actively maintained; Ultraleap hand-tracking hardware remains current | The authors scope it to usernames, passwords, email addresses, and search queries, not long-form text; no controlled WPM result |
| 2022 | TypeAnywhere | QWERTY finger sequence from two Tap Strap devices; BERT-derived language decoder; no key coordinates | Study section reports N=10; five days and 2.5 hours total; 70.6 WPM/1.50% CER table, 43.9 WPM/1.37% CER lap, 87.8 WPM physical baseline | Academic prototype; MIT code repo last pushed 2022, model hosted separately on Drive | Same-finger keys are unobservable; phrase transcription and language prior dominate; lap tap misses and rollover remain; paper has a cohort-count inconsistency noted below |
| 2022-2026 | Tap Strap 2 and TapXR | Proprietary learned finger chords on a surface; TapXR combines motion and optical sensing | Shipped products, first-party firmware through 2024, support/app posts into 2026, and an active owner community; no independent product-wide cohort metric found | Company active; current shop has no products; TapBand successor is waitlist-only and shifts toward smart-glasses control | Month-scale learning, hard and well-lit surface preference, slower than a physical keyboard, and inconsistent legacy-stock availability |
| 2023-2025 | LapCal | 34-key QWERTY glove with finger IMUs and ML | Hardware, code, and limited recordings; see the dedicated evidence audit | Repository is not archived; last remote push 2025-07-22; inactive versus discontinued is unresolved | No valid live key-identity or text-entry result; do not treat it as a completed or formally abandoned product |
| 2024 | TapType | QWERTY finger sequence from two 1.6 kHz wrist accelerometers plus Bayesian and n-gram decoding | Online N=10; 19.2 WPM and 0.6% CER after about 30 minutes | Academic prototype | Same-finger keys come from language, not measured position; personal calibration and hard-surface impacts are required |
| 2024 | StegoType | Egocentric stereo cameras, 30 Hz hand tracking, imagined surface QWERTY | Training: 268K transcriptions from 606 typists; live N=18: 42.4 WPM and 7% UER versus 74.5 WPM and 0.8% on a physical keyboard | Academic prototype | Ring/pinky occlusion, camera/privacy burden, surface registration, and a 7% final error load |
| 2025-2026 | Perceptron-GIK glove | Two-hand glove with five FSR contacts and per-finger IMU features, BLE, and learned key prediction | Public code, a single named data directory, model artifacts, three contributors, and 60 issues; no paper, cohort, WPM, CER, release, or license | Active repository prototype; last push 2026-03-11 | Open issues still cover live demo/inference, preprocessing leakage, calibration, inconsistent FSR thresholds, BLE dropouts, and cross-hand alignment |
| 2024-current | Twiddler 4 | One-handed tactile chorded keyboard and touchpad | Current first-party store page, in stock at US$229; older study reports 26.2 WPM after 400 minutes | Shipped/current | It survives by retaining reliable physical keys and a dedicated niche; it does not meet the no-keyboard interaction goal |

## Closest repository lineage

### LapCal

LapCal is the supplied reference, not the landscape's performance benchmark.
The [evidence audit](lapcal-evidence-audit.md) already establishes its sensor,
data, synchronization, split, firmware, and licensing boundary, so this report
does not repeat it. For status only: GitHub reports no release, two
contributors, three open issues, `archived=false`, and a last push on
2025-07-22. "Abandoned" is therefore a user characterization, not a verified
maintainer statement.

### Perceptron-GIK glove-invisible-keyboard

This Imperial College Applied ML lab repository is the closest current public
descendant found. It is material rather than a README-only idea: it contains
Arduino firmware, two-hand BLE reception, per-finger IMU and FSR features,
alignment code, training and inference paths, a 21.8 MB model, and one named
recording directory. Its issue history is more valuable than its sparse
README. It documents the same hard problems found in LapCal: sensor dropout,
BLE disconnects, inconsistent FSR thresholds, frame transforms, calibration,
left/right synchronization, preprocessing leakage, and a live-demo path that
remains open.

This is evidence that the hardware class remains buildable in 2026. It is not
evidence that the keyboard works. There is no license, release, bill of
materials, documented cohort, session split, WPM, CER, false-activation rate,
or complete demo. A 2026 issue titled "Updating readme so I can put this on the
CV" also supports class-project context rather than product readiness.

### imutype bachelor

The 2017 `imutype/bachelor` repository is an older IMU-and-ML virtual-keyboard
thesis prototype. It has one contributor, one star, no release, no license,
and no push after 2017. It is safely classified as silent. There is no direct
closure statement, so "abandoned" would overstate the evidence.

### TypeAnywhere implementation

`DrustZ/TenFingerTyping` is a small MIT-licensed implementation with decoder
training code and a web demo. Its last push was in 2022, it has no release, and
its model weight is hosted on Google Drive rather than in a versioned release.
The reusable artifact is the finger-sequence decoder and correction interface,
not a complete sensing product.

## Repeated failure modes

| Failure mode | Systems that expose it | Evidence, not inference | Consequence for later decisions |
| --- | --- | --- | --- |
| Same-finger key identity is absent | TypeAnywhere, TapType, PinchType | The sensor emits a finger or finger group; language chooses among every QWERTY key owned by that finger | Sensor-only, language-assisted, out-of-vocabulary, symbols, shortcuts, and source-code tasks need separate scores |
| Correlated fingers create false taps | ATK, glove repositories | ATK measured strong adjacent-finger coupling; Perceptron-GIK tracks FSR thresholds and active-finger errors | Intent and finger identity need continuous-stream tests, not isolated balanced windows |
| No tactile home reference | ATK, SelfieType, StegoType, projection keyboards | ATK documents endpoint overlap; camera and projection community reports ask for tactile reference | A surface does not replace key edges, travel, or home-row landmarks |
| Environment controls performance | Tap/TapXR, projection, optical, radar | Hard and well-lit surfaces recur in owner reports; camera/radar papers document occlusion, clutter, and registration | Surface, lighting, posture, and sensor placement are test factors, not setup footnotes |
| Personal and session calibration | QwertyRing, TapType, LapCal, Perceptron-GIK | Ring orientation, 30 taps per finger, FSR thresholds, and remount transforms are explicit | Cross-day don/doff and remount evaluation is mandatory |
| Correction hides sensor weakness | TypeAnywhere, ATK, TapType, TipText, PinchType, mmKey | Each uses a vocabulary, candidate list, auto-correction, or language model; PinchType rises from 12.54 to 20.07 WPM when correction time is removed | Report raw events, final text, correction actions, and constrained-language coverage separately |
| Wearable burden and learning | Tap Strap, DigiTouch, Gest, Twiddler | TapXR owner preferred its bracelet over finger straps; DigiTouch fit and dense regions caused frustration; Gest required finger bands; Twiddler research needs hours of practice | Comfort, don/doff time, social acceptability, and learning curve can kill an accurate decoder |
| Prototype-to-product financing | Gest | First-party final update names missing production and delivery funding and issued refunds | A demo BOM is not a manufacturable, supportable product cost |
| Repository evidence is incomplete | LapCal, imutype, Perceptron-GIK, TypeAnywhere | No release or live metric, external model artifacts, open leakage/demo issues, or missing license | Code availability cannot substitute for a reproduced live study |

The dominant engineering fork is now explicit. A decoder can avoid geometric
key sensing by accepting a language-dependent finger-sequence product, or it
can preserve arbitrary QWERTY by observing position and accepting a camera,
registered field, or instrumented surface. No reviewed system escapes both
costs.

## Current commercial survivors and counterexamples

### Tap

Tap proves that finger-driven keyboard HID can ship. First-party pages in 2026
still publish support content and a TapModes application, and Tap owners report
using TapXR with mobile XR. The current home page, however, promotes TapBand as
an Apple Watch accessory for smart-glasses commands and offers only a waitlist.
The shop returns "No products were found matching your selection." The correct
status is active company and installed base, with uncertain new TapXR or Tap
Strap availability.

The interaction is also not ordinary QWERTY muscle memory. Users learn chords.
That makes Tap a current product counterexample for event detection and HID
integration, but not validation of an imagined 34-key layout.

### Twiddler 4

Twiddler has survived across generations and is in stock. Its reliable tactile
buttons, handheld body, customization, training tools, and accessibility niche
are precisely the properties removed by an invisible keyboard. Its survival
supports chord learning as a viable niche; it argues against assuming that
removing hardware improves the product.

### Projection keyboards

Canesta, Celluon Epic, Serafim Keybo, and generic laser keyboards show that a
portable visible layout can ship. They are not proof of invisible operation.
Current search results include legacy support and marketplace listings, but no
verified current Celluon or Serafim production statement was found. Community
reports consistently raise bright-light, recognition, and tactile problems.
This category is `shipped/legacy`, not a confirmed current high-volume market.

### Ultraleap

Ultraleap remains a current hand-tracking vendor, and its controller is a
useful optical component. Its XR Keyboard repository is not a maintained text
product: the README explicitly says it is "not actively maintained or managed"
and scopes it to short strings. Treat current tracking hardware and the legacy
keyboard example as separate facts.

## Community pulse

Community material is used only to generate failure cases. It does not replace
a controlled study.

- A February 2026 TapXR owner reported about 30 WPM after a month of constant
  use, faster than a phone but slower than a physical keyboard. The same thread
  says TapXR works best on a flat, well-lit desk, is poor for true on-the-go
  input, and that the learning curve causes users to give up. This is one owner,
  not a cohort.
- A November 2023 TapXR first-impressions post preferred the bracelet form
  factor over Tap Strap 2, described better initial tap accuracy and no noticed
  false positives, but also exposed missing launch firmware features. It is a
  useful indication that wearability can improve without changing the chord
  alphabet.
- A two-device Tap Strap 2 diary reached 14 hours 52 minutes of practice by day
  11 and said errors usually felt user-caused. Later commenters asked whether
  the author continued, without a documented long-term answer. It illustrates
  both deliberate learning and survivorship uncertainty.
- A 2022 XR discussion about laser projection keyboards preferred a foldable
  tactile keyboard, citing focus changes, bright-light weakness, and a poor
  past device experience. The sample is tiny, but its failure cases agree with
  the optical mechanism.

## Claim audit

| Claim | Verdict | Why |
| --- | --- | --- |
| "LapCal is abandoned" | Unverified | Not archived; last push in 2025; no maintainer closure statement found |
| "Gest failed" | Confirmed | First-party final update says the company closed and refunded backers because production funding was unavailable |
| "AirType or Noki was cancelled" | Unverified | A concept and later rebrand are visible, followed by silence; no closure statement or shipment evidence found |
| "Samsung cancelled SelfieType" | Unverified | Samsung documents a CES 2020 C-Lab concept; its official tag has no later result, but silence is not cancellation |
| "Tap is dead" | Contradicted | 2026 app/support activity and TapBand waitlist are live; legacy typing-product stock remains unclear |
| "Ultraleap's keyboard is maintained" | Contradicted | The current repository says it is not actively maintained; current Ultraleap hardware does not change that repository fact |
| "TypeAnywhere proves 70.6 WPM invisible typing" | Too broad | It proves fast English phrase transcription from finger identity plus a neural language model; it does not sense same-finger key positions |
| "ATK proves arbitrary ten-finger air typing" | Too broad | It uses expert typists, word candidates, confirmation gestures, and language correction; raw finger and key ambiguity remains high |
| "Projection keyboards prove no-hardware typing" | Contradicted | The projector and IR plane are physical hardware and the layout is visible |
| "Mouseless is an invisible-keyboard predecessor" | Category error | Its first-party description implements mouse cursor motion and clicks, not text entry |
| "A recent active GitHub repo proves the glove works" | Contradicted | Perceptron-GIK has substantive source but no cohort or live text metric, and its open issues still include leakage, calibration, and live inference |

One primary paper also contains an evidence inconsistency worth preserving.
TypeAnywhere's study section says it recruited 10 participants and its logs,
participant identifiers, and 1,500-phrase count use 10. An introductory
sentence says five participants. This report uses N=10 because it is supported
by the detailed method and arithmetic, but any reuse of the headline metric
should cite the inconsistency rather than silently choose a number.

## Reusable ideas versus misleading precedents

### Reusable

- **Finger sequence as a deliberate product contract.** TypeAnywhere shows
  that abandoning coordinate recovery can produce high phrase-transcription
  throughput. This is reusable only if the language and arbitrary-symbol
  limits are accepted and measured.
- **Separate contact from key decoding.** FSR contact in Perceptron-GIK,
  high-rate wrist impacts in TapType, and optical geometry in StegoType observe
  different variables. Each can be evaluated independently before fusion.
- **Collect ordinary typing, not theatrical gestures.** ATK found correlated
  finger motion and large endpoint overlap. Its behavioral data are a better
  design input than assuming isolated finger taps.
- **Preserve familiar mappings when reducing the layout.** DigiTouch, TipText,
  PinchType, and TypeAnywhere reuse QWERTY finger or spatial groups. They learn
  faster than arbitrary glove chords, even when they cannot preserve full key
  geometry.
- **Treat form factor as a measured subsystem.** TapXR owner reports favor the
  bracelet over finger straps. Don/doff, fit, surface, posture, and false taps
  belong in the same evaluation as CER.
- **Use the newest open glove as failure evidence.** Perceptron-GIK's issues
  provide current test cases for threshold drift, BLE, synchronization,
  leakage, and calibration even before its classifier can be trusted.

### Misleading

- A balanced isolated-key accuracy, tap F1, or model file is not live text.
- WPM is not comparable without cohort, task, vocabulary, correction behavior,
  practice, surface, and final error rate.
- A language model's corrected output is not measured physical key identity.
- A visible projected keyboard or hidden touchscreen is not the same product as
  an unmarked passive surface.
- A live website does not prove stock, shipment, maintenance, or company health.
- A silent repository is not confirmed abandoned, and an unarchived repository
  is not confirmed active.
- Current hand-tracking hardware does not make an old keyboard demo current.

## Source registry

All sources were accessed on 2026-08-09. Grades follow the sensing comparison:
I is peer-reviewed primary work, II is an author manuscript or repository, III
is first-party product material, V is direct project-status evidence, and VI is
community anecdote.

| ID | Grade | Source | Claim boundary |
| --- | --- | --- | --- |
| P1 | I/II | [TypeAnywhere author PDF](https://faculty.washington.edu/wobbrock/pubs/chi-22.03.pdf) | Interaction, decoder, N and its inconsistency, longitudinal table/lap metrics, errors, limitations |
| P2 | II | [TypeAnywhere code](https://github.com/DrustZ/TenFingerTyping) | MIT implementation, external model weight, last push, no release |
| P3 | I/II | [ATK author PDF](https://pi.cs.tsinghua.edu.cn/lab/papers/p539-yi.pdf) | N=8, 23.0 to 29.2 WPM, 0.3% word error, raw ambiguity, Leap Motion setup, candidate UI |
| P4 | I/II | [TapType paper](https://arxiv.org/html/2410.06001) | N=10 online metrics, 1.6 kHz wrist sensing, calibration, Bayesian and language boundaries |
| P5 | I/II | [QwertyRing author PDF](https://pi.cs.tsinghua.edu.cn/lab/papers/QwertyRing.pdf) | Cohorts, five-day metrics, raw position and out-of-vocabulary limitations |
| P6 | I/II | [StegoType author PDF](https://www.keithv.com/pub/stegotype/richardson_stegotype.pdf) | Training corpus, Quest 3 live study, WPM/UER, latency and occlusion |
| P7 | I/II | [DigiTouch author PDF](https://ubicomplab.cs.washington.edu/pdfs/digitouch.pdf) | N=10 longitudinal metrics, glove design, error and fit limitations |
| P8 | I/II | [TipText author PDF](https://teyenwu.com/publications/TipText.pdf) | N=12 performance study, 11.9/13.3 WPM, 0.30% UER, layout and overlay |
| P9 | I | [PinchType DOI](https://doi.org/10.1145/3334480.3382888) | N=14 preliminary interaction and correction-time boundary |
| P10 | III | [Samsung SelfieType announcement](https://news.samsung.com/global/samsung-electronics-to-showcase-successful-c-lab-inside-projects-and-c-lab-outside-start-ups-at-ces-2020) | Official C-Lab concept scope; not a shipment or metric |
| P11 | III/V | [Gest Kickstarter](https://www.kickstarter.com/projects/asdffilms/gest-work-with-your-hands) | Backer count, funding, promised product context |
| P12 | V | [Gest final-update announcement](https://x.com/trygest/status/720071204598587392) | Direct closure, refunds, and production-funding reason |
| P13 | IV/V | [AirType concept report](https://www.parts-people.com/blog/2014/07/18/airtype-loses-the-keyboard-allows-you-to-type-on-any-surface/) and [Noki rebrand post](https://www.facebook.com/Airtyping/posts/1532947113609376/) | Concept mechanics and rebrand only; neither proves shipment or closure |
| P14 | III | [TapBand current page](https://www.tapwithus.com/) and [Tap firmware changelog](https://www.tapwithus.com/changelog/) | Active 2026 pivot/waitlist and legacy firmware activity |
| P15 | III | [Tap shop](https://www.tapwithus.com/shop/) | No products currently returned; not proof of company closure |
| P16 | II/V | [Ultraleap XR Keyboard](https://github.com/ultraleap/XR-Keyboard) | Open-source short-string example and explicit not-maintained disclaimer |
| P17 | III | [Twiddler 4 store](https://www.tekgear.com/twiddler-4-wrap.html) | Current in-stock price and tactile chorded product scope |
| P18 | II/V | [Mouseless first-party archived page](https://www.pranavmistry.com/archived/projects/mouseless/) | IR mechanism, US$20 prototype, publications, mouse-only scope, archive status |
| P19 | II/V | [Perceptron-GIK repository](https://github.com/Perceptron-GIK/glove-invisible-keyboard) and [issues](https://github.com/Perceptron-GIK/glove-invisible-keyboard/issues) | Current code topology, activity, missing product evidence, open engineering blockers |
| P20 | II/V | [imutype bachelor repository](https://github.com/imutype/bachelor) | Older IMU glove prototype and silent repository status |
| P21 | II/V | [LapCal repository](https://github.com/xjjak/LapCal) | Remote status only; technical claims stay in the local audit |
| P22 | I | [Typing on an Invisible Keyboard DOI](https://doi.org/10.1145/3173574.3174013) | Hidden-layout touchscreen metric and instrumented-surface boundary |
| P23 | I/II | [mmKey DOI](https://doi.org/10.1109/JIOT.2021.3084560) | Radar key/word results and registered-surface task |
| P24 | VI | [TapXR 2026 mobile-owner thread](https://www.reddit.com/r/Xreal/comments/1qwwxzv/alternatives_to_traditional_mousekeyboard_tapxr/) | One-owner learning, speed, surface, lighting, and mobility anecdotes |
| P25 | VI | [TapXR first impressions](https://www.reddit.com/r/TapWithUs/comments/17upjas/tapxr_first_impressions/) | One-owner form-factor and initial false-positive comparison |
| P26 | VI | [Two-Tap learning diary](https://www.reddit.com/r/TapWithUs/comments/oglc30/my_journey_with_two_tap_strap_2s_two_handed_typing/) | Practice duration and incomplete long-term follow-up |
| P27 | VI | [Projection-keyboard XR thread](https://www.reddit.com/r/nreal/comments/yho65v/anyone_tried_using_laser_keyboard_with_nreal/) | Tactile, focus, lighting, and poor-device anecdotes |
| P28 | Local audit | [LapCal evidence audit](lapcal-evidence-audit.md) | Reproducibility and no-live-keyboard boundary; not repeated here |

## Research coverage and limitations

- Kagi authentication was verified. Kagi Assistant was the primary synthesis
  lane, Kagi Search supplied discovery, and Kagi Extract read first-party pages
  and primary papers. Assistant claims were checked against the sources; this
  caught its incorrect characterization of Ultraleap XR Keyboard as maintained.
- Gitquarry was verified against `github.com` with a saved token. Native search
  ran before discover mode. It found the active Perceptron-GIK repository, the
  older imutype thesis, LapCal, TypeAnywhere code, and the Ultraleap example.
  Repository trees, code, push dates, releases, licenses, and issue titles were
  used instead of popularity as proof.
- Reddit discovery began with Kagi `site:reddit.com`, then structured post and
  comment data were read for four threads. Anecdotes are explicitly grade VI.
- The earlier Perplexity 403 and gpt2agent Turnstile failures were not retried.
- AirType/Noki has no preserved first-party technical page in the reviewed
  sources. Its sensing description is secondary, and its final state is
  unknown.
- No sales data establish broad or failed adoption for Tap, Twiddler, or
  projection keyboards. Availability, community use, and adoption are separate
  claims.
- Patent scope and freedom to operate remain with ticket 008. This report names
  systems, not patent conclusions.

## Highest-impact implication for the remaining tickets

The next decisions must not ask only "which sensor recognizes keys best?" They
must first select which information contract is acceptable:

- If English language reconstruction and weak arbitrary-symbol support are
  acceptable, finger identity plus a TypeAnywhere-like decoder is the strongest
  published throughput precedent and avoids absolute key localization.
- If arbitrary text, commands, code, and per-key truth are required, those
  headline finger-sequence WPM figures are the wrong benchmark. A geometry
  channel or instrumented surface must earn its camera, registration, privacy,
  and hardware cost.

Whichever branch remains open, ticket 005 should require free composition,
out-of-vocabulary text, symbols, shortcuts, correction actions, raw sensor-only
output, language-assisted output, cross-day remounts, ordinary-motion false
activations, and long-form fatigue. Those measurements are the recurring
difference between a convincing demo and a usable keyboard.
