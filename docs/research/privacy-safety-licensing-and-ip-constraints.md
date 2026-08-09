# Privacy, safety, licensing, and IP constraints

Access date: 2026-08-09. Source-integrity recheck: 2026-08-29.

## Decision

Neither conditional sensing co-lead is ruled out for a controlled, local
prototype. Both require design gates before they may emit operating-system
input:

1. The dual-wrist IMU path is the lower-privacy lead. It should keep raw motion,
   inferred fingers, text, and language-model state on the local host. It still
   handles secrets and can become a behavioral-biometric system if later used
   to identify people.
2. The egocentric optical path can observe physical key geometry, but it also
   creates the highest bystander and environment-capture burden. It must crop to
   hands and the registered surface before recording, show an unambiguous
   capture indicator, and avoid retaining raw frames by default.

The first prototype may use Linux `uinput` behind a deterministic, fail-neutral
adapter. USB HID is the cross-platform appliance proof. BLE HOGP is a later
interoperability and security stage, not the first transport. Production BLE,
USB marks, batteries, body-worn radio, and market access have qualification and
test costs that the research BOM does not settle.

The project sets one hard artifact-reuse boundary: the canonical implementation
must not import LapCal code, PCB layouts, CAD, datasets, model artifacts, or
calibration values. Its root applies CC BY-NC-SA 4.0 to "this work," has no
separate commercial grant, and grants no patent or trademark rights. This is a
conservative project policy, not a statement of the full license grant.
BY-NC-SA permits specified noncommercial exercises of the Licensed Rights under
its conditions, and it does not govern a use for which copyright or similar
rights permission is not required. Inspect and cite LapCal as prior-art and
failure evidence, then implement from project-authored requirements.

This report is architecture and risk research. It is not legal advice, a
freedom-to-operate opinion, a product certification plan, a medical assessment,
or approval to run a human-subject study.

## Scope and unresolved product facts

The conclusions cover the two current co-leads, optional acoustic or sEMG
research, local language correction, Linux `uinput`, USB HID, and BLE HOGP.
They assume a non-medical consumer or research input device.

The following facts remain human decisions and prevent a final legal or
certification answer:

- countries where research, sale, import, cloud processing, and support occur;
- whether the prototype stays private, recruits participants, or is distributed;
- whether a camera, microphone, electrodes, or cloud language model is allowed;
- whether the product identifies users or only decodes their current input;
- whether it is sold as accessibility, health, rehabilitation, or medical tech;
- radio module, antenna, battery, charger, materials, enclosure, and final host;
- use of Bluetooth or USB names, logos, and assigned identifiers;
- business model, controller/processor roles, retention, and third-party vendors.

An internal bench rig and a finished product do not have the same obligations.
Using a pre-certified radio module also does not automatically certify the final
body-worn host, antenna integration, emissions, or RF exposure.

## Data and threat model

### Assets

- raw IMU, image, depth, radar, sEMG, force, and optional audio streams;
- calibrated hand geometry, typing rhythm, device and participant identifiers;
- tap, finger, key, modifier, shortcut, and corrected-text event streams;
- passwords, messages, code, health data, and other content entered through it;
- language-model context, dictionaries, personalization state, and model updates;
- BLE bonds, firmware signing keys, update metadata, and host permissions;
- participant consent, manifests, deletion records, and study results.

### Trust boundaries

```text
body / room
    -> sensor firmware
    -> USB serial or authenticated BLE research service
    -> local decoder and optional language model
    -> fail-neutral key-state adapter
    -> uinput, USB HID, or BLE HOGP
    -> focused operating-system application

optional research capture
    -> encrypted, armed session store
    -> approved analysis workspace
    -> aggregate report or deleted raw session
```

The sensor and decoder are not trusted merely because they are local. A stolen
device, malicious host process, compromised update, exposed debug port, false
BLE peer, or accidental diagnostic build can cross these boundaries.

### Principal attackers and failures

- a nearby radio attacker pairs, reconnects, or reads an unauthenticated service;
- malware with access to sensor logs, model state, `/dev/uinput`, or update keys;
- a curious developer enables raw logging and later syncs the folder to cloud;
- a compromised model or firmware update injects keys or weakens capture rules;
- a crash, disconnect, sleep, or update leaves Shift, Control, or Delete down;
- a participant or bystander is recorded outside the consented task or period;
- an inference artifact memorizes phrases or associates a motion template with a
  named person;
- copied research code, weights, or data contaminates a commercial codebase;
- a production design enters a patent claim or market without scoped review.

## Privacy and security requirements

### Data classification

Raw motion is not harmless. It can reveal activity, handedness, timing, and
typing behavior. Optical and depth frames can include hands, faces, documents,
screens, rooms, and bystanders. Audio can capture speech. sEMG is a physiological
signal. Every modality can yield inferred text, which is at least as sensitive
as the application receiving it.

Under GDPR Article 4(1), sensor or text data tied to an identifiable participant
is personal data. Article 4(14) defines biometric data as personal data from
specific technical processing of physical, physiological, or behavioral
characteristics that allows or confirms unique identification. Article 9(1)
adds the special-category restriction for biometric data processed for the
purpose of uniquely identifying a person. Therefore:

- wrist motion used only to decode the current user's keys is not automatically
  Article 9 biometric identification, but it is still personal data;
- a typing-cadence or motion template used to recognize who is typing crosses
  the biometric-identification boundary;
- optical hand tracking may create hand-geometry data even without an identity
  feature, so its treatment under narrower US statutes is jurisdiction-specific;
- typed content can itself expose Article 9 categories such as health, politics,
  religion, or sex life even when the sensor stream is not biometric.

On-device processing reduces disclosure and breach surface. It does not make
personal-data processing disappear. A shipping design still needs a controller,
purpose, lawful basis, notice, retention rule, security measures, and rights
workflow where GDPR applies. A high-risk or biometric-identification deployment
may require a DPIA under Article 35; counsel or the responsible privacy officer
must decide from the actual processing and scale.

US biometric definitions are not interchangeable:

- Illinois BIPA expressly enumerates retina/iris scans, fingerprints,
  voiceprints, and scans of hand or face geometry. An optical hand-geometry
  pipeline is a counsel gate in Illinois even if its product purpose is typing.
  An IMU trace is not one of the enumerated identifiers, but a later identity
  feature or combined geometry pipeline changes the analysis.
- Texas Business and Commerce Code Chapter 503 enumerates comparable biometric
  identifiers captured for a commercial purpose. Do not assume that a GDPR
  classification answers Texas scope.
- Washington RCW 19.375 defines an identifier as automatic measurement of a
  biological characteristic used to identify a specific person. It excludes a
  photograph, video, audio recording, and data generated from them from that
  definition, while separately regulating enrollment, disclosure, and retention.
- California Civil Code 1798.140 includes physiological, biological, and
  behavioral characteristics in biometric information and treats biometric
  information processed to uniquely identify a consumer as sensitive personal
  information. Exact CCPA/CPRA duties depend on business applicability and use.

This does not mean ordinary motion data is unregulated in the US. General state
privacy, data-breach, consumer-protection, contract, wiretap/recording, and
research rules can apply even when a biometric statute does not.

### Required product defaults

1. Process locally. No sensor, text, focused-app, clipboard, correction-context,
   or language-model telemetry leaves the device by default.
2. Keep production logs content-free. Metrics may contain counts, stage latency,
   loss, firmware/model version, and coarse failure codes. They must not contain
   raw frames, inferred keys, surrounding text, app titles, stable motion
   embeddings, or persistent participant identifiers.
3. Arm research capture explicitly. Display a hardware-visible or
   operating-system-visible recording state, the modalities active, destination,
   and remaining retention period. Capture stops on lock, logout, or consent
   withdrawal.
4. Separate stores. Raw sensor, labels, decoded events, corrected text, app
   acknowledgements, consent, and aggregate metrics have distinct files and
   access rules. Never put a password or secure-field test into any of them.
5. Use generated secrets. Secure-field testing records only expected/observed
   lengths, event counts, and a local pass/fail result. Do not log or screenshot
   plaintext.
6. Minimize camera data before storage. Crop or mask outside the registered hand
   volume on the capture device where possible. Do not record room audio for an
   optical experiment.
7. Give deletion teeth. Each session has an owner, purpose, expiration, deletion
   method, derived-artifact map, and deletion record. A backup or trained model
   that cannot honor deletion is a design defect, not an exception.
8. No identity reuse. Calibration IDs are random study IDs. Do not use motion,
   hand geometry, sEMG, typing rhythm, or embeddings for authentication,
   analytics identity, fraud scoring, or cross-service linkage.
9. Keep language correction local and bounded. Do not send prompts, partial
   words, code, or surrounding context to a hosted model without a separately
   approved product decision, vendor review, notice, and no-training contract.
10. Make telemetry opt-in, granular, revocable, and off for raw or inferred
    content. Consent to operate the keyboard is not consent to improve a model.

### BLE, firmware, debug, and host controls

HOGP requires a security and interoperability review, not merely a successful
connection. The Bluetooth Core Security Manager defines pairing,
authentication, encryption, bonding, and key distribution. A displayless
wearable can fall back to an unauthenticated pairing association and therefore
lack man-in-the-middle protection. The prototype must record the association
model and achieved security properties rather than labeling every encrypted
link "authenticated."

- Require an intentional physical pairing gesture and a short discoverable
  window. Do not auto-pair while worn.
- Prefer LE Secure Connections with an authenticated association such as
  numeric comparison, passkey, or a designed out-of-band route when both hosts
  support it. If HOGP interoperability forces Just Works, state the MITM limit,
  bind pairing to physical presence, and prohibit raw research services on that
  bond.
- Separate HID from a research GATT service. The latter requires authorization,
  freshness/sequence checks, and encryption; it is absent from production.
- Provide a physical clear-bonds operation. Rate-limit pairing and expose the
  active host without exposing a stable tracking identifier in advertisements.
- Sign firmware and model updates, bind them to product/version, verify before
  install, prevent unauthorized rollback, survive power loss, and retain a known
  recovery image. Updates start from and return to an all-keys-up state.
- Protect signing keys outside source control and build logs. Separate release
  signing from developer access and record reproducible hashes.
- Keep SWD/JTAG/UART available on clearly marked research units only. Production
  policy must disable or authenticate debug access and prevent debug logs from
  bypassing privacy controls. Do not claim that a readout-protection fuse alone
  secures secrets without target-specific validation.
- Give the decoder no shell, window-control, clipboard, browser, or network
  privilege. A small adapter owns `/dev/uinput`; the model process submits only
  validated key-state messages over a local authenticated/permissioned channel.
- Allowlist advertised keys and chords. Never map model output to command
  strings, scripts, paste buffers, or arbitrary HID descriptors.
- Begin neutral, reject duplicate transitions, keep one owner of down-key state,
  release all keys on planned stop, and use a host/device watchdog to reach
  application-observed neutral state within the Gate 0S deadline.

NISTIR 8259A supplies a useful voluntary baseline: device identification,
configuration, data protection, logical access to interfaces, secure software
update, and cybersecurity-state awareness. ETSI EN 303 645 adds a consumer-IoT
baseline including no universal default passwords, vulnerability reporting,
secure update, protected sensitive data, minimized attack surfaces, software
integrity, deletion, and resilient operation. Compliance with either is not a
substitute for the target market's law or a threat model.

## Physical and product safety

No source establishes that tapping a desk without key travel is safer than a
keyboard. The product changes contact force, wrist posture, hand hovering,
correction load, repetition, and visual attention. The 90-minute feasibility
gate is a first screen, not proof of long-term safety.

| Hazard | Paths | Prototype control | Expert or certified gate |
| --- | --- | --- | --- |
| Repetition, awkward wrist posture, hand hovering, and fatigue | All; impact is strongest for surface IMU/optical | Neutral posture instructions, adjustable surface/height, breaks, workload and discomfort sampling, stop on pain, numbness, weakness, or loss of control | Human-factors/ergonomics review before extended or occupational claims; longitudinal study before a safety benefit claim |
| Repeated fingertip impact and hard/rough surfaces | Surface IMU and optical | Smooth, stable, clean surface; no sharp edges; measure task time and discomfort; do not coach harder taps to rescue accuracy | Ergonomist and participant protocol; compare force/exposure with a normal keyboard before claiming reduced strain |
| Band pressure, heat, sweat, abrasion, and allergy | Wrist IMU, sEMG, rings | Multiple sizes, rounded cleanable enclosure, dry skin, short sessions, inspect skin before/after, material declarations | Skin-contact/material specialist; nickel release and chemical review for markets such as EU REACH; biocompatibility plan if intended use makes it applicable |
| Adhesive/electrode irritation, current path, and cross-contamination | sEMG | Use intact skin, single-user consumables, vendor instructions, trained operator, isolation, stop on irritation | Ethics/IRB-equivalent review for participants; electrical/biological safety expert; medical-device counsel if claims or intended use cross that line |
| Lithium cell fire, swelling, puncture, overcharge, or thermal burn | Wearable wireless paths | Reputable protected cell, matched charger, current/temperature protection, strain relief, nonconductive enclosure, no charging while worn in early tests | IEC 62133-2 cell/battery evidence, IEC 62368-1 product safety assessment, abuse/charging/thermal lab tests; UN 38.3 evidence before transport |
| USB power, ESD, short circuit, cable snag, or connector failure | Tethered and appliance paths | Current-limited source, fuse/protection, insulated boards, strain relief, neutral reset on disconnect | IEC 62368-1 and EMC/ESD lab plan for a finished product |
| Body-worn RF exposure and antenna detuning | BLE wrist/ring | Use an authorized low-power module within integration conditions; fixed antenna and enclosure; measure current/power | FCC portable/body-worn RF exposure and target-market lab assessment; module grant review does not cover every host integration |
| Mechanical breakage, small parts, ingress, and sharp edges | Wearables | Fastener retention, rounded enclosure, inspection, sweat barrier, drop and snag tests | Mechanical reliability, ingress, flammability, and age-grade review before sale |
| False shortcuts, destructive input, or loss of control | All OS/HID paths | Harmless test target, allowlisted events, emergency disable, all-up watchdog, ordinary-movement negatives | Safety/security review before system-wide use; application-observed fault injection at Gates 0S, 5, 8, and 9 |
| Exclusion or harm from accessibility claims | All | Preserve StickyKeys/FilterKeys and host repeat; disclose touch-typing, movement, vision, surface, and learning requirements | Co-design and test with the claimed disabled populations; accessibility and claims review before marketing |

OSHA and NIOSH ergonomics guidance identifies repetition, awkward posture, and
contact stress as musculoskeletal risk factors. It does not certify this novel
interaction. IEC 62368-1:2023 is a hazard-based safety standard for audio/video,
ICT, and communication equipment; IEC 63203-406-1 defines contact-surface
temperature measurement for wrist-worn devices. The exact adopted national
edition and conformity path require a lab after the enclosure, battery, use
position, and market are frozen.

Do not make rehabilitation, injury-prevention, medical, or universal-access
claims from a usability study. Such claims can change the evidence standard and
regulatory classification.

## License and provenance map

### LapCal

The pinned root [`README.org`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org)
says, "This work is licensed under" CC BY-NC-SA 4.0, and its
[`LICENSE.md`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/LICENSE.md)
contains that legal code. No root language separates software, firmware, PCB,
CAD, data, or models. The conservative project rule is therefore to treat every
original tracked artifact as covered unless its own file clearly states
otherwise.

Subject to its terms, CC BY-NC-SA 4.0 permits exercise of the Licensed Rights to
reproduce and share licensed material, and to produce, reproduce, and share
adapted material, for noncommercial purposes. Sharing requires attribution,
license notice/link, modification notice, and ShareAlike for adapted material.
The license defines NonCommercial as not primarily intended for or directed
toward commercial advantage or monetary compensation. The boundary is
purpose-specific and not equivalent to "free while pre-revenue." Where an
exception or limitation applies and permission is unnecessary, the CC license
does not apply. Section 2(b)(2) expressly does not license patent or trademark
rights. Privacy, publicity, and third-party rights can also remain.

Creative Commons says its licenses are not recommended for software or
hardware. That poor fit creates interpretation and compatibility risk; it does
not turn the repository into permissively licensed material.

Artifact decisions:

| Material | Decision | Reason |
| --- | --- | --- |
| LapCal prose, diagrams, photos, source, PCB/Gerber, and original CAD | Project policy: permit inspection and citation for evidence, but exclude artifact reuse in the canonical implementation | Permission for a particular reproduction or adaptation depends on the Licensed Rights, license terms, and applicable exceptions or limitations; no patent or trademark rights are granted |
| LapCal datasets and trained TFLite/model files | Project policy: do not train, benchmark, ship, or redistribute | No separate subject consent, dataset license, model provenance, or commercial grant; a repository license cannot supply participant rights |
| LapCal algorithms and lessons | Project policy: re-express facts and lessons as functional requirements and tests | Ideas and measured facts can inform a new design, but avoid copied expression, structure, constants, layouts, CAD, or weights; jurisdictional exceptions need counsel |
| LapCal Round-Anything submodule | MIT source may be used separately with its notice | Its own MIT license covers the dependency; it does not relicense LapCal's ring design or adaptations |

### Other relevant artifacts

| Artifact | Verified license boundary | Reuse decision |
| --- | --- | --- |
| `facebookresearch/emg2qwerty` repository | Root is CC BY-NC-SA 4.0; GitHub archived it read-only on 2026-08-01; the paper reports limited broader access to its proprietary research-grade sEMG hardware | Paper and source are study material. Do not copy code, data, configuration, or weights into a commercial path without a separate grant and participant/provenance review |
| `DrustZ/TenFingerTyping` implementation | Root MIT license, copyright Mingrui Zhang | Code can be reused with the copyright and permission notice, but its external Google Drive model weight is not covered merely because the code is MIT. Do not use that weight without its own provenance and license |
| Academic papers, PDFs, and performance tables | Publication access is not a software, dataset, CAD, model, or patent license | Cite ideas and facts; do not copy supplemental artifacts unless their own terms allow it |
| Unlicensed GitHub prototypes, including repositories with no root license | Copyright remains reserved by default | Study public behavior and independently implement; do not copy source, assets, PCB, CAD, or models |
| Vendor SDKs, HID stacks, hand trackers, and pretrained models | Terms vary by component, platform, field of use, redistribution, and model | Create a software/materials bill of materials and approve every transitive runtime, model, and dataset before freezing the prototype |

### Independent-implementation provenance policy

1. Keep this dossier as the evidence and requirements record. It records
   observable behavior, hazards, tests, source citations, and known prior art
   without importing LapCal implementation text into product artifacts.
2. Write a fresh protocol, data schema, calibration procedure, key-state
   machine, PCB requirements, and mechanical constraints from the selected
   components and current standards.
3. Use project-authored requirements and vendor datasheets as implementation
   inputs. If a contributor inspects LapCal for evidence, record that source and
   do not copy its code, PCB, CAD, data, model, calibration values, or protected
   expression into product artifacts.
4. Record provenance for every source file, CAD element, footprint, model,
   training session, participant, dataset, font, and dependency in an SBOM and
   data/model bill of materials.
5. Use fresh participants, consent, immutable raw sessions, and a newly trained
   model. Never initialize from unknown or NC weights.
6. Run a similarity and provenance review before any public or commercial
   release. Ask LapCal's rights holders for a separate commercial license only
   if copying a specific artifact would materially outperform reimplementation.

This policy documents independent creation and reduces artifact-copying risk.
It is not a legal safe harbor and does not establish a strict clean-room
process. It does not resolve patents.

## Targeted patent landscape

This search used project names, "invisible keyboard," "keyboard-less typing,"
"gesture keyboarding," wrist/IMU, camera/optical, hand geometry, and air-gesture
terms. Records were checked on Google Patents against their named assignee,
priority, and displayed legal status. Google itself warns that displayed legal
status and anticipated expiration are assumptions, not legal conclusions.

| Record/family lead | Assignee shown | Priority/status shown | Relevance and limit |
| --- | --- | --- | --- |
| [US7042442B1, Virtual invisible keyboard](https://patents.google.com/patent/US7042442B1/en) | IBM | 2000-06-27; expired, fee-related; displayed expiry 2021-03-23 | Camera plus gesture classes and key probabilities is close optical prior art. A displayed expired US member does not prove every foreign or continuation right is dead |
| [US20100199228A1, Gesture Keyboarding](https://patents.google.com/patent/US20100199228A1/en) | Microsoft | 2009-01-30; abandoned | Gesture recognition mapped to OS/application control is prior art, not an active US exclusion right from this application. Related family members still need checking |
| [US20230195306A1, Software for keyboard-less typing based upon gestures](https://patents.google.com/patent/US20230195306A1/en) | Individual | 2014-09-01; pending continuation | Broadly adjacent contact/gesture keyboardless typing. Pending claims can change, and its continuation chain requires file-history and family review |
| [US12299194B2, Systems and methods for gaze-assisted gesture control](https://patents.google.com/patent/US12299194B2/en) | Meta Platforms Technologies | 2023-01-27; active | Current AR/VR gaze-plus-gesture control. It is adjacent to an optical/headset product, but title/abstract proximity does not establish claim overlap |
| [US20250321646A1, Air gesture user interfaces](https://patents.google.com/patent/US20250321646A1/en) | Apple | 2024-04-15; pending | Current air-gesture UI family, broader than surface typing. Pending and not a reason to abandon a desk-surface prototype without claim mapping |
| [WO2026018256A1, Gesture-based typing for smart devices](https://patents.google.com/patent/WO2026018256A1/en) | Individual | 2024-07-16; pending PCT publication | Recent typing-specific lead. National-phase entry, claims, owners, and territory are unresolved |

No specific TapType patent family was confirmed in this bounded pass. That is
not evidence that none exists. Tap Systems also advertises patented technology,
but marketing language is not a patent-family map.

Before design freeze, patent counsel should search CPC/IPC classes and inventor,
assignee, citation, continuation, and national-family links for the exact frozen
architecture. Counsel must map live independent claims to sensor placement,
calibration, feature extraction, language decoding, displayed layout, surface
registration, correction, and HID output. They must verify ownership,
assignments, prosecution history, maintenance fees, terminal disclaimers,
expiry adjustments, and target territories.

A patent can be invalid, expired, not entered in a country, or not read on the
product even when its abstract sounds close. Conversely, a search miss can have
claims that read on the design. This landscape is not an FTO opinion.

## Product and radio obligations

| Surface | Prototype position | Pre-market gate |
| --- | --- | --- |
| US FCC | Use an FCC-authorized BLE module, listed antenna/configuration, and the module grant conditions. A private wired bench path avoids adding a transmitter | Intentional radiators generally require certification before marketing under FCC equipment-authorization rules. Review host integration, Part 15 emissions, labeling, RF exposure for a body-worn portable device, and whether module conditions cover the final enclosure/antenna |
| EU/EEA CE | Do not claim conformity for an unfinished rig | A radio product needs the RED 2014/53/EU conformity assessment, technical file, declaration, marking, safety/health, EMC, and spectrum evidence. Determine RoHS, WEEE, battery, REACH, and General Product Safety Regulation duties from the final product and economic operators |
| EU radio cybersecurity | Use NIST/ETSI controls now so the architecture can comply later | Delegated Regulation (EU) 2022/30 applies RED Article 3(3)(d), (e), and (f) requirements to defined classes from 1 August 2025. Whether the keyboard can communicate over the internet, processes personal/traffic/location data, or supports money/monetary value decides which clauses apply. EN 18031 conformity and restrictions require a qualified EU review |
| Great Britain UKCA/CE | No market claim | Radio Equipment Regulations 2017 apply in GB. Current acceptance of CE marking, UKCA options, designated standards, importer duties, and transitional rules must be checked at launch; Northern Ireland follows a separate route |
| Bluetooth SIG | Prototype only under development tools and module terms; do not market or use the Bluetooth marks as certification | Bluetooth SIG says all Bluetooth products must be qualified by the member company before sale/distribution. As of 1 March 2026, Adopter membership is USD 0 and a first design qualification fee is USD 12,000; later products using the same design may not repeat that fee. Verify the exact referenced design and product scenario |
| USB-IF | Use a development VID/PID only under its grant; do not put a certified logo on the rig | USB-IF logos require compliance testing, current Integrators List entry, a company VID, and the trademark license. The official page lists a USD 3,500 logo administration fee for nonmembers, waived for members. A working HID descriptor is not USB certification |
| Battery transport | Prefer cabled power for the first human session | Portable lithium cells/batteries need supplier evidence and product integration review. UN Manual of Tests and Criteria subsection 38.3 applies before transport of a battery type; shipping mode and carrier rules are separate |

The product owner must retain an accredited EMC/radio/safety lab after the
schematic, module, antenna, enclosure, charging system, firmware, and markets
are stable. Testing too early produces evidence for the wrong product.

## Prioritized risk register

Likelihood and impact are planning estimates before prototype evidence. `H`,
`M`, and `L` mean high, medium, and low. Gates refer to the feasibility contract.

| Priority | Risk | L/I | Evidence | Required mitigation | Owner | Gate | Residual uncertainty |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Password, message, code, or health content enters logs, telemetry, or hosted correction | H/H | A keyboard necessarily sees inferred text; OS report already forbids password training/logging; GDPR content can expose special categories | Content-free production logs, generated secure-field tests, local correction, no network egress, explicit separate opt-in | Privacy + decoder | Before Gate 0A; audit at Gates 3-5 and 9 | Frameworks and third-party crash reporters can log strings outside the adapter |
| 2 | Unauthorized or destructive key injection through host privilege, radio, model, or update | M/H | `uinput` and HID emit system input; NIST baseline calls for interface access, data protection, and secure update | Privilege-separated adapter, local permissioned protocol, event allowlist, authenticated pairing, signed updates, no shell/clipboard/window APIs | Security + input adapter | Gate 0S and Gate 5 | OS-specific privileged surfaces and supply-chain compromise remain |
| 3 | Stuck modifier/repeat after crash, disconnect, sleep, or update | M/H | Input-path report defines neutral start and all-up within 250 ms | One key-state owner, duplicate rejection, watchdog, all-up on stop/update, application-observed fault tests | Firmware + input adapter | Gate 0S; repeat at Gates 5 and 9 | USB removal can prevent an explicit all-up report; host must observe neutral/device removal |
| 4 | Optical frames capture bystanders, screens, rooms, or identities | H/H for optical; L/L for IMU | Optical co-lead sees the environment; GDPR and recording laws can apply before biometric use | On-device crop/mask, no audio, visible capture state, controlled room, bystander exclusion/notice, raw frames off by default | Privacy + optical | Before any capture; Gates 0A and 9 | Headset/vendor services may retain diagnostics outside project control |
| 5 | Motion, hand geometry, sEMG, or cadence becomes identity data | M/H | GDPR purpose/identification test; BIPA expressly includes scans of hand geometry; state definitions differ | Prohibit identity/authentication use, random session IDs, no stable embeddings, DPIA/counsel gate for changed purpose | Privacy/legal + ML | Data design; Gate 6/7 | Models can encode identity even without an explicit identity head |
| 6 | BLE Just Works link is encrypted but not MITM-authenticated; stale bonds enable reconnect | M/H on BLE | Bluetooth Security Manager association determines authentication; HOGP adds pairing/bonding complexity | Physical pair window, strongest supported LE Secure Connections association, clear bonds, separate research service, document achieved properties | Firmware/security | Gate 0S and appliance proof | Host HOGP UX may force unauthenticated association |
| 7 | Debug interface or unsigned/rollback update bypasses privacy and key controls | M/H | NIST/ETSI require secure update and controlled interfaces | Release signing, anti-rollback policy, power-loss recovery, protected keys, production debug policy, neutral update | Firmware/release | Before wearable human use; Gate 0S fault tests | MCU-specific readout/debug bypass resistance needs lab/vendor evidence |
| 8 | LapCal or NC research artifacts contaminate commercial implementation | H/H if reused | Local root CC BY-NC-SA; patent rights excluded; no data/model grant | Independent implementation, no-artifact-reuse policy, fresh code/PCB/CAD/data/model, provenance ledger, counsel before any exception | Project lead/legal | Before implementation and every release | Scope and copyrightability are fact/jurisdiction-specific |
| 9 | Unlicensed model weights, datasets, footprints, SDKs, or participant data enter build | M/H | TypeAnywhere code is MIT but external weight terms are not established; papers do not license artifacts | SBOM plus data/model BOM, per-artifact license and consent evidence, hashes, reject unknown provenance | ML + release | Before training; before distribution | Transitive SDK/model terms and generated-output rights can change |
| 10 | Active or pending patent claims read on frozen sensing/decoder flow | M/H | Targeted records include a pending keyboardless continuation and active/pending gesture UI families | Counsel-led FTO after architecture freeze; claim chart, family/status/territory/prosecution review; design around or license | Project lead/patent counsel | Before design freeze or investment beyond prototype | Search is non-exhaustive; claims and status change |
| 11 | Battery, thermal, charging, or RF hazard injures a wearer | M/H | CPSC battery hazards; IEC 62133-2/62368-1; FCC body-worn exposure | Start tethered, certified cell/module, protection and enclosure, no early charging while worn, safety/RF lab plan | Hardware/safety | Before Gate 9 wearable run; pre-market | Final antenna, enclosure, charger, and misuse determine result |
| 12 | Repetition, impact, posture, or fatigue causes harm or biases results | H/M | OSHA/NIOSH identify repetition, awkward posture, contact stress; no safety proof for desk tapping | Conservative sessions/breaks, stop symptoms, comfort/workload tracking, ergonomics review, never require harder taps | Human factors | Gate 9; before extended study | Long-term exposure and population variability remain unknown |
| 13 | Product is sold without FCC/RED/GB/Bluetooth/USB/product-safety work | H/H if launch is rushed | Official market and qualification rules; Bluetooth fee is material | Freeze markets/configuration, hire lab/compliance owner, budget qualification, keep marks off prototypes | Product/compliance | Pre-market release gate | Exact routes depend on product, radio, marks, and economic operators |
| 14 | "Accessibility" or injury-prevention claims exclude or harm intended users | M/H | Input requires touch-typing, motion, vision/surface, learning; host accessibility settings affect events | Preserve OS settings, publish limitations, disabled-user co-design, claim-specific evidence, no medical claims | Product/accessibility | Gates 5 and 9; marketing review | Needs target population and use context |
| 15 | Deleted participant data persists in backups, models, or language dictionaries | M/H | GDPR storage limitation/rights and Washington retention principle; derived artifacts can encode sessions | Expiry manifest, derived-artifact graph, retrain/delete procedure, backup TTL, deletion verification | Data steward/ML | Before first participant | Machine-unlearning adequacy and legal retention vary |
| 16 | False confidence from an authorized module, standard, or paper replaces final testing | H/M | Module grants have conditions; standards are scoped; papers test different tasks | Maintain applicability matrix, named configuration, lab reports, application-observed tests, explicit unknowns | Systems/compliance | Every stage | Regulations and harmonized/designated standards change |

## Gates and owners

### Before collecting any participant data

- Name controller/data steward, purpose, lawful basis/consent, modalities,
  recipients, retention, deletion, and withdrawal path.
- Complete a privacy and bystander review; escalate a DPIA if the actual design
  meets the applicable high-risk criteria.
- Approve every dataset/model/source license and participant-rights record.
- Use a harmless target application and generated strings. No real passwords.
- Approve an ergonomics and stop-symptom protocol. Start without a battery.

### Before enabling system input

- Pass deterministic Gate 0S through Linux `uinput` with privilege separation,
  allowlisted events, stage timestamps, ten forced faults, and neutral recovery.
- Show that crash, cable/radio loss, sleep, focus change, and update cannot leave
  a destructive held state in the target application.
- Keep the gesture decoder disconnected from input until stream integrity and
  quiet-period false activations are measured.

### Before a BLE or USB appliance proof

- Record pairing association, authentication, encryption, bond lifecycle, GATT
  exposure, update signature/rollback, debug policy, and power-loss recovery.
- Re-run Gates 0S, 5, 8, and 9 through the final transport.
- Use development identities/marks lawfully; do not imply FCC, Bluetooth, USB,
  CE, or UKCA certification.

### Before distribution or commercial investment

- Freeze intended use, claims, markets, radio/antenna, power, enclosure,
  materials, firmware/model versions, cloud vendors, and supported OS paths.
- Obtain privacy and patent counsel scoped to those facts.
- Commission the applicable safety, EMC, radio, RF exposure, chemical/material,
  battery, transport, interoperability, and accessibility work.
- Complete Bluetooth qualification if marketed with Bluetooth; obtain lawful
  USB identifiers and logo rights if using USB marks.
- Publish retention/deletion, security update, vulnerability reporting, and
  support-lifetime commitments.

No new Wayfinder ticket is needed yet. The remaining human architecture,
decoder, and decisive-prototype tickets already own the facts that determine
which privacy, counsel, lab, and qualification branches become real.

## Source registry

Web sources were first accessed 2026-08-09. The LapCal, Creative Commons,
Copyright Act, and emg2qwerty boundaries were rechecked 2026-08-29. Official
law and regulator pages define the cited boundary. Standards catalogue pages
establish scope, not a certificate. Patent status from Google Patents is a
discovery aid and must be checked against each patent office.

| ID | Source | Boundary used |
| --- | --- | --- |
| R1 | [Immutable LapCal `README.org`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/README.org), [`LICENSE.md`](https://github.com/xjjak/LapCal/blob/8638759168d84d32fa50ae461e053055a01c3dff/LICENSE.md), and the [pinned local snapshot](../../research/LapCal/README.org) | Root "this work" statement; CC BY-NC-SA legal text, NC/SA/attribution, no patent or trademark grant |
| R2 | [Creative Commons FAQ](https://creativecommons.org/faq/) | CC does not recommend CC licenses for software; license interpretation is not legal advice |
| R3 | [CC BY-NC-SA 4.0 legal code](https://creativecommons.org/licenses/by-nc-sa/4.0/legalcode.en) | Canonical definitions, grant, conditions, other rights, termination |
| R4 | [Round-Anything license](../../research/LapCal/CAD/rings/Round-Anything/LICENSE) | MIT terms for that submodule only |
| R5 | [emg2qwerty license](https://github.com/facebookresearch/emg2qwerty/blob/main/LICENSE) and [NeurIPS paper](https://proceedings.neurips.cc/paper_files/paper/2024/file/a64d53074d011e49af1dfc72c332fe4b-Paper-Datasets_and_Benchmarks_Track.pdf) | Repository root CC BY-NC-SA 4.0; physical-keyboard data collection and limited access to proprietary research-grade hardware |
| R6 | [TenFingerTyping license](https://raw.githubusercontent.com/DrustZ/TenFingerTyping/master/LICENSE) | MIT code license; does not establish external weight terms |
| R7 | [GDPR official text, Regulation (EU) 2016/679](https://eur-lex.europa.eu/eli/reg/2016/679/oj/eng) | Articles 4, 5, 6, 9, 13-17, 25, 32, and 35 |
| R8 | [Illinois Biometric Information Privacy Act](https://www.ilga.gov/Legislation/ILCS/Articles?ActID=3004&ChapterID=57) | Enumerated identifiers, notice/consent, retention, disclosure, enforcement |
| R9 | [Texas Business and Commerce Code Chapter 503](https://statutes.capitol.texas.gov/Docs/BC/htm/BC.503.htm) | Texas biometric-identifier duties and commercial-purpose scope |
| R10 | [Washington RCW 19.375](https://app.leg.wa.gov/RCW/default.aspx?cite=19.375&full=true) | Identification purpose, recording exclusions, enrollment/disclosure/retention |
| R11 | [California Civil Code 1798.140](https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=CIV&sectionNum=1798.140.) | Biometric-information and sensitive-personal-information definitions |
| R12 | [NISTIR 8259A](https://csrc.nist.gov/pubs/ir/8259/a/final) | Voluntary IoT device cybersecurity capability baseline |
| R13 | [ETSI EN 303 645 V3.1.3](https://www.etsi.org/deliver/etsi_en/303600_303699/303645/03.01.03_60/en_303645v030103p.pdf) | Consumer IoT security provisions |
| R14 | [Bluetooth Core 5.4 Security Manager](https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Core-54/out/en/host/security-manager-specification.html) | Pairing, authentication, encryption, bonding, and association behavior |
| R15 | [Bluetooth HID over GATT Profile](https://www.bluetooth.com/specifications/specs/hid-over-gatt-profile-hogp/) | BLE HID service/profile boundary |
| R16 | [Linux kernel `uinput`](https://docs.kernel.org/input/uinput.html) | Userspace virtual-input capability and event path |
| R17 | [FCC equipment authorization procedures](https://www.fcc.gov/general/equipment-authorization-procedures) | Certification/SDoC procedures and intentional-radiator boundary |
| R18 | [FCC RF safety](https://www.fcc.gov/general/radio-frequency-safety-0) | Human RF exposure framework; final portable/body-worn assessment remains product-specific |
| R19 | [Radio Equipment Directive 2014/53/EU](https://eur-lex.europa.eu/eli/dir/2014/53/oj/eng) | EU radio safety/health, EMC, spectrum, conformity and marking framework |
| R20 | [Delegated Regulation (EU) 2022/30](https://eur-lex.europa.eu/eli/reg_del/2022/30/oj/eng) | RED Article 3(3)(d)-(f) classes and application from 1 August 2025 |
| R21 | [Great Britain Radio Equipment Regulations 2017 guidance](https://www.gov.uk/government/publications/radio-equipment-regulations-2017/radio-equipment-regulations-2017-great-britain) | GB economic-operator, conformity, and marking route |
| R22 | [Bluetooth product qualification](https://www.bluetooth.com/develop-with-bluetooth/qualify/) and [fees](https://www.bluetooth.com/fee-schedule/) | Qualification before market and effective 2026 fees |
| R23 | [USB-IF logo license](https://www.usb.org/logo-license) | Compliance/Integrators List/VID conditions and nonmember fee for marks |
| R24 | [IEC 62368-1:2023](https://webstore.iec.ch/en/publication/69308) | Hazard-based AV/ICT product safety scope |
| R25 | [IEC 63203-406-1:2021](https://webstore.iec.ch/en/publication/62634) | Wrist-worn contact-surface temperature test method |
| R26 | [IEC 62133-2](https://webstore.iec.ch/en/publication/70017) | Portable sealed lithium-cell/battery safety requirements and tests |
| R27 | [UN Manual of Tests and Criteria, revision 8 files](https://unece.org/transport/dangerous-goods/rev8-files) | Lithium battery transport test framework including subsection 38.3 |
| R28 | [CPSC battery topic](https://www.cpsc.gov/Regulations-Laws--Standards/Voluntary-Standards/Topics/Batteries) | Consumer high-energy battery hazard context |
| R29 | [OSHA keyboard ergonomics](https://www.osha.gov/etools/computer-workstations/components/keyboards) | Awkward posture, repetition, and contact-stress controls |
| R30 | [NIOSH ergonomics risk factors](https://www.cdc.gov/niosh/ergonomics/ergo-programs/risk-factors.html) | Work-related musculoskeletal risk-factor process |
| R31 | [ECHA nickel restriction guidance](https://echa.europa.eu/documents/10162/17233/nickel_restriction_prolonged_contact_skin_en.pdf) | Direct/prolonged skin-contact nickel release concern |
| R32 | [USB HID 1.11](https://www.usb.org/sites/default/files/hid1_11.pdf) | HID report/state protocol; not product certification |
| R33 | [Patent records listed in the targeted landscape](#targeted-patent-landscape) | Titles, assignees, priority dates, and displayed status with explicit limits |
| R34 | [17 U.S.C. Section 102(b)](https://www.copyright.gov/title17/92chap1.html#102) | US idea, procedure, process, system, method, concept, principle, and discovery exclusion; not a project-specific safe harbor |

## Source and tool limits

- Kagi authentication was available. Kagi Assistant supplied the first triage;
  Kagi Search discovered primary sources; Kagi Extract read official pages and
  legal text where supported. Assistant assertions were not accepted when the
  primary source did not support them.
- EUR-Lex GDPR, Texas statutes, and FCC pages returned incomplete or empty Kagi
  Extract bodies in some calls. Their official URLs remain the cited source;
  exact launch advice requires direct statutory and counsel review.
- Google Patents is convenient family/status discovery, not an authoritative
  legal-status service. USPTO, EPO/WIPO, national registers, file histories, and
  assignments were not exhaustively searched.
- No patent claims were charted. No FTO, invalidity, noninfringement, or expiry
  conclusion is made.
- No laboratory tested RF, EMC, battery, temperature, skin contact, repetitive
  strain, security, or accessibility behavior. Standards applicability is a
  pre-market work list, not proof of conformity.
- No participant consent, data-processing agreement, vendor contract, or model
  card was available for review. Paper availability never filled those gaps.
