# Cost viable prototype paths

Access date: 2026-08-09

## Decision boundary

This is an estimate-only procurement screen. No item was bought, reserved, or
uploaded, and no vendor account was created. It does not choose a sensing
architecture. Prices are prototype quantities, not production unit costs.

The cheapest evidence-producing path is a tethered, host-decoded pair of Seeed
XIAO nRF52840 Sense boards. A complete build with one spare can be planned at
USD 95 minimum, USD 180 realistic, or USD 310 upper bound before tax. Its
minimum and realistic cases fall below the report's dated USD 230.70
sensitivity input for the historic EUR 200 cash ceiling. That spend can falsify
stream, contact, and finger-evidence assumptions. It cannot prove exact
arbitrary QWERTY key identity because wrist IMUs do not observe key position.

Optical tracking is also cheap when compatible hardware and compute are already
owned. All-new optical, custom IMU, radar with raw capture, magnetic scientific,
and sEMG paths exceed that dated sensitivity input. Labor and participant
payments dominate every credible study, so a parts-only ceiling must not be
presented as a project budget.

## Cost rules and assumptions

- Public prices and availability were captured on 2026-08-09. USD is used
  unless the source lists another currency.
- On 2026-08-09 this report recorded a planning conversion stated for
  2026-08-07: EUR 1 = USD 1.1535, or USD 230.70 for EUR 200. The cited ECB
  daily feed is a moving current-rate endpoint and does not independently
  reproduce that historic value. Treat USD 230.70 as a dated planning input,
  not a verified historical-rate fact [P16].
- Listed prices exclude sales tax, VAT, customs, and shipping unless noted.
  RobotShop says its USD 217 Leap Motion Controller 2 price includes US tariffs
  but calculates shipping at checkout [P7]. OpenBCI says shipping is calculated
  at checkout. Planning envelopes include a stated shipping allowance but still
  exclude tax and customs.
- Minimum assumes a host computer, phone or camera for reference video, basic
  soldering tools, USB hub, 3D printer access, and ordinary lab test equipment
  are already owned. Realistic includes one working spare and ordinary fixtures.
  Upper includes a second spare or build spin and missing bench tools.
- Planning allowances for straps, fabrication, fixtures, shipping, and
  contingency are not vendor quotes. Requote them after the design, destination,
  and tax status are known.
- Engineering effort is shown as hours and, only to make totals comparable, at
  an illustrative loaded rate of USD 100/hour. Replace that rate with the actual
  internal or contractor rate.
- Participant allowance uses USD 25/hour plus 0-25 percent scheduling and
  incidentals. This is a planning assumption for in-person sensor work. It is
  above Prolific's USD 8/hour minimum and USD 12/hour recommendation for online
  work [P15], but it is not an IRB-approved universal rate.
- Free/open tools are assumed for firmware, capture, analysis, Linux `uinput`,
  USB HID, and BLE HOGP. A private lab prototype has no app-store publication,
  driver-signing, Bluetooth qualification, certification, or cloud fee. Those
  product costs remain excluded and unknown.

## Envelope summary

Cash includes parts, consumables, a shipping allowance, and contingency, but
not tax/customs, participants, or labor.

| Path | Cash min / realistic / upper | Participant allowance | Engineering | Illustrative all-in min / realistic / upper | Decision bought |
| --- | --- | --- | --- | --- | --- |
| Host-decoded dual-wrist IMU | USD 95 / 180 / 310 | Unknown until paid Gate 0B-2 hours are scheduled | 60-140 h | Unknown; USD 6,095 / 10,180 / 14,310 before participant cost | Whether the full two-wrist stream can detect taps and fingers reliably enough to continue claim A |
| Cleaner custom dual-wrist IMU | USD 295 / 650 / 1,520 | USD 500-1,500 | 140-320 h | USD 14,795 / 24,650 / 35,020 | Whether placement, power, radio, and repeatable mounting survive a purpose-built revision |
| Optical/IR, compatible hardware owned | USD 60 / 250 / 650 | USD 500-2,000 | 120-300 h | USD 12,560 / 21,250 / 32,650 | Whether owned cameras observe contact and layout geometry under intended lighting and occlusion |
| Optical/IR, all-new | USD 320 / 950 / 2,500 | USD 500-2,000 | 120-300 h | USD 12,820 / 21,950 / 34,500 | Same geometry decision without relying on owned hardware |
| USB HID appliance increment | USD 0-40 / 150 / 350 | USD 0-300 | 20-80 h | USD 2,000 / 5,250 / 8,650 | Whether frozen events survive USB enumeration, faults, down/up, modifiers, repeat, and target apps |
| BLE HOGP appliance increment | USD 25 / 180 / 500 | USD 0-500 | 60-180 h | USD 6,025 / 11,380 / 19,000 | Whether pairing, bonds, radio faults, sleep, latency, and battery meet the same event contract |
| Radar, board-only to raw-capture lane | USD 240 / 1,450 / 3,500 | USD 250-1,000 | 160-450 h | USD 16,490 / 29,950 / 49,500 | Whether a current 3-RX/2-TX EVM can resolve adjacent registered keys; not whether it reproduces mmKey |
| Magnetic scientific lane | USD 300 / 1,500 / 7,000 | USD 0-1,000 after bench success | 250-700 h | USD 25,300 / 47,000 / 78,000 | Whether multi-finger geometry is measurable in a useful volume before wearable form-factor work |
| sEMG research lane | At least USD 5,768 listed floor, unavailable; realistic and upper unknown | USD 500-3,000 | 300-900 h | Unknown | Whether a purchasable, synchronized, sufficient-rate bilateral system contains keyboardless intent evidence |
| Optional Pi Zero 2 W gateway | USD 15-55 / 120 / 300 | USD 0 | 40-120 h | USD 4,015 / 8,120 / 12,300 | Whether MCU profiling proves a larger local decoder is required |

Where shown, the illustrative all-in midpoint uses the midpoint engineering
hours and a representative participant allowance. It is arithmetic for
comparison, not a quote. The host-decoded IMU row stops at cash plus engineering
because the canonical Gate 0B-2 protocols do not specify enough paid hours to
price their participant work. It implies no participant total.

## 1. Cheapest host-decoded dual-wrist IMU falsification rig

Use two tethered XIAO nRF52840 Sense boards and decode on an owned desktop. The
board combines nRF52840, native USB/BLE, and an LSM6DS3TR-C IMU. Seeed listed it
at USD 15.90 and in stock [P1]. One spare is included in minimum and realistic
cases. The configured sample rate, packet integrity, and useful wrist-impact
bandwidth must be measured; the product page does not prove TapType-equivalent
1.6 kHz performance.

| Item | Minimum | Realistic | Upper |
| --- | ---: | ---: | ---: |
| XIAO Sense boards | 3 x 15.90 = 47.70 | 47.70 | 4 x 15.90 = 63.60 |
| Data cables | Owned | 3 x 6.50 allowance = 19.50 | 26.00 |
| Wrist straps, foam, fasteners, small cases | 15.00 | 50.00 | 39.00 |
| Independent contact/reference fixture | 10.00 | 20.00 | 50.00 |
| USB hub/extension and power measurement | Owned | 0.00 | 35.00 |
| Batteries, only for radio/power follow-up | 0.00 | 0.00 | 4 x 6.95 = 27.80 [P5] |
| Consumables | 10.00 | 15.00 | 25.00 |
| Shipping allowance | 12.00 | 25.00 | 40.00 |
| Rounding/contingency | 0.30 | 2.80 | 3.60 |
| **Cash** | **95.00** | **180.00** | **310.00** |

Substitute: a separate Pololu LSM6DSO carrier is USD 19.95, active, with
backorders allowed [P4]. It is useful when a different MCU is already owned,
but two carriers plus MCUs cost more than the integrated XIAO pair. Stop after
Gate 2 if exact key identity is required and same-finger keys remain
unobservable.

## 2. Cleaner custom dual-wrist revision

Do this only after the cheap rig passes its physical gate. Use a certified
nRF52840 module or a small native-USB/BLE controller, an explicitly selected
high-rate IMU, protected charging, a real power switch, timestamp/sync input,
and repeatable strap datums. A Feather nRF52840 Express is an in-stock USD 24.95
prototype substitute with native USB, BLE, and battery charging [P2]. The
Adafruit LSM6DS3TR-C breakout is USD 9.95 and reported in stock at DigiKey
[P12]. The Nordic nRF52840 DK is USD 48.95 at DigiKey [P11].

Minimum arithmetic is USD 100 PCBA allowance + 48.95 DK + 20.85 batteries +
60 straps/enclosures + 20 harnesses + 20 consumables + 25 shipping = USD
294.80, rounded to USD 295. Realistic is one assembled spin plus spares and
fixtures at USD 650. Upper is two spins, missing rework/debug tools, and extra
mechanicals at USD 1,520.

JLCPCB advertises bare prototypes from USD 2 and assembly from an USD 8 setup
fee, but its own material says the exact total depends on stencil, joints,
manual placement, extended parts, and selected components [P13][P14]. Those
headline prices are not a board quote. Do not order until schematics, radio
layout, battery safety, test pads, and deletion criteria for the dev boards are
frozen.

## 3. Optical and IR geometry

### Already-owned hardware

Cash covers mounts, stable surface/layout registration, controlled lighting,
privacy screening, cables, and an independent reference. Minimum is USD 60;
realistic is USD 250; upper is USD 650 with a robust overhead fixture and a
spare current camera. The camera/headset and host remain owned equipment, not
zero-value BOM items. Record their model, purchase status, and replacement cost.

### All-new hardware

- Sensor plus existing host: RobotShop listed Leap Motion Controller 2 at USD
  217 including US tariffs, with shipping at checkout [P7]. Add mounting,
  registration, privacy screening, and shipping for USD 320 minimum.
- Self-contained headset research: Meta listed Quest 3 512 GB at USD 599.99
  [P8]. Add a cleanable interface, strap/power accessories, case, and fixtures
  for about USD 950 realistic.
- A new camera, new suitable workstation, reference camera, fixtures, privacy
  barriers, and spares can reach USD 2,500. The workstation is an allowance,
  not a selected product.

Ultraleap states that a Leap Motion Controller 2 purchase includes a commercial
camera-use license [P6]. That removes a prototype license line; it does not
waive future distribution, privacy, platform, or third-party model obligations.
Do not buy optical hardware before the camera/headset product boundary is
decided.

## 4. USB HID and BLE HOGP increments

The existing XIAO sensor MCU or another native-USB MCU may emit USB HID, making
the minimum incremental part cost USD 0-40. A dedicated, replaceable appliance
uses the in-stock USD 24.95 Feather nRF52840 Express [P2]. Two boards, two
cables, enclosures, a fault fixture, and shipping total about USD 150 realistic;
three boards, powered hub, power analyzer, robust fixture, and contingency total
about USD 350 upper.

BLE can use the same nRF52840. Minimum adds two USD 6.95 batteries and a safe
pairing/reset affordance, rounded to USD 25 [P5]. Realistic is USD 180 for two
test boards, three batteries, controls, enclosures, one host adapter, power
measurement, and shipping. A broader host/pairing lab is USD 500 upper.

No custom Windows/macOS virtual-HID driver or signing certificate is needed for
a standards HID appliance. Product Bluetooth qualification, radio compliance,
battery transport testing, app signing, and certification are excluded. BLE
spend is premature until USB Gate 0S passes; a gateway is premature until the
frozen decoder fails profiling on the sensor MCU.

## 5. Radar feasibility board

TI listed IWRL6432BOOST at USD 155; its official page identifies 57-64 GHz,
3 RX, 2 TX, USB power, and optional DCA1000 raw-ADC capture [P9]. DigiKey search
showed buy-now/ships-today and a distributor index reported 26 units at USD
156.25 [P10]. A board, mount, registration materials, shipping, and contingency
make a USD 240 minimum point-cloud experiment.

Raw capture changes the cost. Mouser EU listed DCA1000EVM at EUR 849.65 [P17];
the US page blocked automated extraction, while an authorized-distributor index
showed USD 987.96. I therefore use USD 987.96 only as a planning input, not a
confirmed checkout quote. IWRL6432BOOST + DCA1000 + network/storage, fixtures,
cables, shipping, and contingency is about USD 1,450 realistic. Two radar
boards, one capture board, storage/network upgrades, absorbers, mounts, spares,
and 25-30 percent uncertainty are USD 3,500 upper.

The SDK is downloadable without a listed software fee, but TI marks some SDK
downloads as requiring export approval [P9]. The spend buys only an
adjacent-key localization test. The current board is not equivalent to mmKey's
unavailable custom aperture. Do not buy DCA1000 until processed output or a
small borrowed-board capture proves raw ADC is required.

## 6. Magnetic scientific lane

There is no purchasable Finexus kit. A cheap field probe can start with six
Adafruit TLV493D three-axis magnetometer boards, including two spares, at
6 x USD 5.95 = USD 35.70; Adafruit showed them in stock [P18]. Add controller,
frequency-separated emitters, drivers/amplifiers, wire, fixtures, cabling,
shipping, and contingency for USD 300 minimum, assuming oscilloscope, bench
supplies, and fabrication tools are owned.

A realistic multi-finger scientific build allocates USD 1,500 across selected
higher-rate magnetometers/ADCs, ten emitters/spares, low-noise drivers, timing,
calibration fixtures, cabling, and a second mechanical iteration. Missing test
equipment and two complete spins raise the upper envelope to USD 7,000.

These are design allowances because component choice depends on range, field,
rate, multiplexing, and metal environment. Polhemus Patriot is a commercial
comparison but lists no public price, supports only two sensors at 60 Hz, and
asks customers to contact sales [P19]. It is not a five-finger substitute. Do
not recruit participants until a bench volume resolves multiple emitters with
the intended keyboard spacing.

## 7. sEMG research lane

The public-price floor is not a viable procurement plan:

- two OpenBCI Cyton + Daisy 16-channel boards: 2 x USD 2,499 = USD 4,998,
  both shown sold out [P20];
- four 10-lead cable packs: 4 x USD 54.99 = USD 219.96, sold out [P21];
- ten gel-electrode packs: 10 x USD 24.99 = USD 249.90, sold out [P22]; and
- USD 200 fit/skin-prep/isolation supplies plus USD 100 shipping allowance.

The total is USD 5,767.86 before tax, rounded to at least USD 5,768. OpenBCI
documents only 125 Hz live streaming for 16-channel Cyton+Daisy [P23], far
below emg2qwerty's 2 kHz research topology. Two units also create a new
synchronization problem. The price floor therefore is neither available nor
technically equivalent.

Delsys Trigno Centro supports up to 32 synchronized sensors, but Delsys exposes
quote/demo requests rather than a public price [P24]. Minimum, realistic, and
upper fit-for-purpose totals remain unknown until a vendor confirms channel
count, sample rate, raw access/SDK, synchronization, sensor count, lead time,
license, and return terms. Do not replace unknown with an industry anecdote or
buy a lower-rate system merely to fill the table.

## 8. Optional gateway

Raspberry Pi lists Zero 2 W at USD 15 [P25]. With owned storage/power/cables,
that is the minimum. A data card, power supply, OTG adapter, enclosure, spare,
and shipping make USD 55-120. A second complete gateway, debug fixture, power
measurement, and robust storage make USD 300 upper. Budget 40-120 engineering
hours for boot, clocks, update/rollback, watchdog, neutral HID state, and power.
Buy it only after measured RAM, flash, inference latency, and power show that
the chosen decoder cannot live on the sensor MCU.

## EUR 200 sensitivity

Using the report's dated USD 230.70 planning conversion, the historic ceiling
passes these cash-only cases:

- dual-wrist IMU minimum and realistic;
- optical minimum when compatible camera/headset and compute are owned;
- USB HID minimum and realistic;
- BLE HOGP minimum and realistic; and
- the optional gateway, which is not a sensing path.

It fails custom IMU, all-new optical, decisive radar, magnetic scientific, and
sEMG. A bare USD 155 radar board is below the ceiling, but its USD 240 minimum
experiment is not. The complete Gate 0B-2 human studies also include unpriced
participant work and engineering outside this cash-only ceiling. The ceiling is
useful as an early hardware stop-loss, not as a product or research-program
budget.

## Gate-linked spend ladder and stop-losses

| Gate | Release only this spend | Participant allowance | Stop-loss |
| --- | --- | --- | --- |
| Before 0A | USD 0: product boundary, claim A/B/C, privacy, surface, and target platform decisions | 0 | Do not buy custom PCBs, camera/headset, DCA1000, magnetic/sEMG hardware, gateway, or BLE polish |
| 0A acquisition integrity | One complete cheap topology: IMU USD 95-180 or owned optical USD 60-250; native capture and deterministic fault fixture | 0 | Stop on silent loss, clock ambiguity, bad sync, or a failed full-topology schedule |
| 0S deterministic output integrity | Reuse the host and Linux `uinput` harness; add only an unowned harmless target/fault fixture | 0 | Stop on an untraceable event, stuck state, missed neutral deadline, or status before durable evidence |
| 0B observable contact | Fund the second matching hand before the canonical round, plus independent truth and replacements; no custom revision | Not priced: schedule paid hours from the frozen roster first | Stop if independent truth cannot align or the contact miss, latency, recall, extra-event, or false-activation rules fail |
| 1 acting finger | Frozen capture/analysis only; no new modality | Not priced: protocol fixes events and exposure, not paid hours | Stop if acting-finger evidence or required negative-condition abstention fails |
| 2 exact 34-position/Core-28 identity | Frozen capture/analysis only; no custom revision yet | Not priced: protocol fixes events and mounts, not paid hours | Stop the exact-input claim when same-finger position is physically unobservable |
| 3 live uncorrected text | Host decoder and test harness; USD 100-500 incremental | Not priced: schedule the two sessions and 1,000 characters first | Stop if only replay/offline works or phrase knowledge leaks in |
| 4 corrected phrase A | Locked local language assets; normally USD 0 license | Not priced: schedule the fixed phrase blocks first | Stop if corrected output cannot be traced or is presented as exact-input evidence |
| 5 arbitrary text B | USB HID appliance only after Gate 3; USD 0-350 | Not priced: schedule the advertised-event trials first | Stop on hidden unsupported keys, repaired identifiers, or unsafe modifier state |
| 6 session/remount transfer | Cleaner wearable or robust optical fixture only now: USD 295-1,520 incremental by lane | Not priced: schedule all three sessions first | Stop if the held-out remount changes normalization/model before scoring or transfer bounds fail |
| 7 person transfer, if claimed | No new modality; freeze it | Not priced: schedule the unseen-person study first | Stop the generality claim if the participant-level confidence bound fails |
| 8 ordinary-movement negatives | Wearable spares, logging, travel/setting supplies | 30 h x 25 = 750, plus incidentals | Stop the always-on claim above 0.1 false activations/hour or on any destructive chord |
| 9 extended use | Batteries and final mechanical cleanability/comfort revision | 12 x 3 h x 25 = 900, plus 20 percent = 1,080 | Stop on adverse event, persistent pain/numbness, collapse, unsafe recovery, or unmeasured runtime |

Some sessions may satisfy more than one gate, but the protocol must be frozen
before claiming that saving. Never recruit the later cohort before the earlier
physical gate passes. Gates 0B through 7 do not specify enough paid duration to
support a cumulative participant budget, so this report does not publish one.
Price each frozen roster and schedule before authorization. Gate 8 and Gate 9
are the only rows whose current protocols state enough participant duration for
the arithmetic shown above.

## Premature purchases

Before Gate 0A and the product-boundary decisions, all of these are premature:

- a custom wearable PCB or enclosure;
- any new Quest, Leap camera, or privacy-sensitive optical rig when cameras are
  not yet accepted;
- DCA1000, a second radar board, or RF accessories;
- a Finexus replica, Polhemus quote, or multi-channel sEMG quote/order;
- a gateway before MCU profiling;
- BLE pairing polish before USB event semantics pass;
- Windows/macOS virtual-HID drivers, signing, app-store membership, Bluetooth
  qualification, compliance testing, or production tooling; and
- later-gate participant recruitment.

## Source and drift audit

| ID | Source | Price/availability fact used |
| --- | --- | --- |
| P1 | [Seeed XIAO nRF52840 Sense](https://www.seeedstudio.com/Seeed-XIAO-BLE-Sense-nRF52840-p-5253.html) | USD 15.90, in stock; onboard IMU and BLE |
| P2 | [Adafruit Feather nRF52840 Express](https://www.adafruit.com/product/4062) | USD 24.95, in stock; native USB, BLE, charging |
| P3 | [Adafruit Feather nRF52840 Sense](https://www.adafruit.com/product/4516) | USD 39.50, out of stock; useful substitute status, not costed |
| P4 | [Pololu LSM6DSO carrier](https://www.pololu.com/product/2798) | USD 19.95, active, backorders allowed |
| P5 | [Adafruit 400 mAh LiPo](https://www.adafruit.com/product/3898) | USD 6.95, in stock |
| P6 | [Ultraleap Hyperion overview](https://docs.ultraleap.com/hand-tracking/Hyperion/index.html) | Controller 2 purchase includes commercial camera-use license |
| P7 | [RobotShop Leap Motion Controller 2](https://www.robotshop.com/products/ultraleap-leap-motion-controller-2) | USD 217 including US tariffs; shipping at checkout |
| P8 | [Meta Quest 3](https://www.meta.com/quest/quest-3/) | Quest 3 512 GB USD 599.99 |
| P9 | [TI IWRL6432BOOST](https://www.ti.com/tool/IWRL6432BOOST) | USD 155 search listing; 57-64 GHz, 3 RX/2 TX, DCA support; inventory requires login |
| P10 | [DigiKey IWRL6432BOOST](https://www.digikey.com/en/products/detail/texas-instruments/IWRL6432BOOST/21296838) | Buy-now/ships-today search result; exact page extraction blocked |
| P11 | [DigiKey nRF52840 DK](https://www.digikey.com/en/products/detail/nordic-semiconductor-asa/NRF52840-DK/8593726) | USD 48.95 search listing |
| P12 | [DigiKey Adafruit LSM6DS3TR-C breakout](https://www.digikey.com/en/products/detail/adafruit-industries-llc/4503/16637690) | USD 9.95; 139 in stock in captured result |
| P13 | [JLCPCB prototype service](https://jlcpcb.com/features/pcb-prototype) | Bare prototype headline from USD 2 |
| P14 | [JLCPCB assembly FAQ](https://jlcpcb.com/help/article/pcb-assembly-faqs) | Assembly headline from USD 8; exact quote depends on parts/process |
| P15 | [Prolific participant pay](https://www.prolific.com/resources/how-much-should-you-pay-research-participants) | USD 8/hour minimum, USD 12/hour recommendation for its online pool |
| P16 | [ECB daily reference rates](https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml) | Moving current-rate feed; it does not preserve or reproduce the report's recorded 2026-08-07 planning value |
| P17 | [Mouser EU DCA1000EVM](https://eu.mouser.com/ProductDetail/Texas-Instruments/DCA1000EVM?qs=MLItCLRbWszpQLhOmNMP6Q%3D%3D) | EUR 849.65 captured search listing; direct automation denied |
| P18 | [Adafruit TLV493D](https://www.adafruit.com/product/4366) | USD 5.95, in stock |
| P19 | [Polhemus Patriot](https://polhemus.com/all-trackers/patriot) | Two sensors, 60 Hz, contact-sales/no public price |
| P20 | [OpenBCI Cyton + Daisy](https://shop.openbci.com/products/cyton-daisy-biosensing-boards-16-channel) | USD 2,499, sold out |
| P21 | [OpenBCI snap cables](https://shop.openbci.com/products/snap-electrode-cables-emg-ecg-eeg) | Ten leads USD 54.99, sold out |
| P22 | [OpenBCI gel electrodes](https://shop.openbci.com/products/kendall-emg-ecg-foam-solid-gel-electrodes-50-pack) | USD 24.99 per pack, sold out |
| P23 | [OpenBCI product relationship FAQ](https://docs.openbci.com/FAQ/HowProductsGoTogether/) | Cyton + Daisy streams 16 channels at 125 Hz |
| P24 | [Delsys Trigno Centro](https://delsys.com/product/trigno-centro/) | Up to 32 synchronized sensors; quote required/no public price |
| P25 | [Raspberry Pi Zero 2 W](https://www.raspberrypi.com/products/raspberry-pi-zero-2-w/) | Official USD 15 price |

Price coverage is strongest for IMU boards, appliance boards, Quest, and
OpenBCI listed-but-sold-out parts. It is weaker for TI raw capture because TI
hides price/inventory behind login and Mouser blocked direct automation.
Magnetic commercial and Delsys sEMG systems are quote-only. Fabrication,
shipping, customs, tax, participant recruiting, workstation choice, and
certification are allowances or unknowns rather than invented point prices.

Kagi authentication was checked and available. Kagi Assistant was the primary
triage lane; Kagi Search and Extract checked manufacturer and distributor pages,
followed by direct HTTP checks where useful. Perplexity remained unavailable
from the earlier expired-session result and gpt2agent remained blocked by
Turnstile; neither was retried. Every price is drift-prone. Recheck stock,
currency, destination, lead time, tax, shipping, and terms immediately before
an authorized purchase.

## Implication for architecture and prototype tickets

Cost does not break the conditional tie between IMU and optical. It does make
the order clear: use the USD 95-180 IMU falsification rig when claim A and a
hard surface are acceptable, or spend USD 60-250 on fixtures when compatible
optical hardware is already owned and claim B geometry matters. Do not blend
those into one BOM. Radar, magnetic, and sEMG must earn their cost with a
modality-specific bench gate. USB HID follows a frozen live decoder; BLE and a
gateway follow only after USB and MCU profiling. Architecture selection must
still be made by the human-owned product-boundary ticket.
