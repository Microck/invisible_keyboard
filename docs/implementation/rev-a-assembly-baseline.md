# Rev-A one-hand planned assembly baseline

Date: 2026-08-12
Last revised: 2026-08-30

## Purpose and status

This is the planned build specification for the first one-hand, wired
falsification rig and the template for its future as-built record. No rig has
been assembled and no hardware gate has run. The specification does not prove
that the rig is electrically reliable, safe to wear, or able to identify keys.
Those claims remain behind the hardware gates in this file and the
[first-prototype roadmap](first-prototype-roadmap.md).

Rev A uses:

- one Seeed Studio XIAO nRF52840 Plus, SKU 102010672;
- one Adafruit TCA9548A breakout, product 2717;
- six Adafruit LSM6DS3TR-C breakouts, product 4503;
- five finger sensors and one dorsal-hand reference sensor;
- six direct INT1 lines and six separately selectable TCA downstream branches,
  with deselected branches disconnected but no galvanic isolation; and
- USB power and data, with no battery or radio.

The typing surface remains passive, unmarked, and sensorless.

## How to read the claims

Every build statement belongs to one of three classes:

- **Source-verified fact:** an inspected vendor page, schematic, drawing, or
  datasheet states it.
- **Rev-A engineering choice:** this document freezes it so different builds
  are comparable. It is not a vendor claim or physical result.
- **Hardware gate:** the assembled rig must measure it before the next step.

No hardware gate in this document has run. Do not relabel a planned check as a
pass.

## Frozen topology

```text
XIAO 3V3 -- PWR_LINK [OPEN] -----+---------------------------+
                                 |                           |
                            TCA VIN                    six sensor VIN
XIAO GND ------------------------+---------------------------+
                                 |                           |
                            TCA GND                    six sensor GND

XIAO D4/D5 -> TCA SDA/SCL -> channel 0 SC0/SD0 -> thumb SCL/SDA
                         -> channel 1 SC1/SD1 -> index SCL/SDA
                         -> channel 2 SC2/SD2 -> middle SCL/SDA
                         -> channel 3 SC3/SD3 -> ring SCL/SDA
                         -> channel 4 SC4/SD4 -> pinky SCL/SDA
                         -> channel 5 SC5/SD5 -> dorsal SCL/SDA

thumb INT1  -> D0                 ring INT1   -> D3
index INT1  -> D1                 pinky INT1  -> D6
middle INT1 -> D2                 dorsal INT1 -> D7

TCA RST -> no connection; the breakout's onboard 10K pull-up holds it high
D8/P1.13 -> TP_SNAPSHOT, active-high Gate 0A aggregate snapshot marker
D9/P1.14 -> TP_COMMIT, active-high Gate 0A cache commit marker
```

Only the standard XIAO edge pins D0 through D9, 3V3, and GND are used. Rev A
does not solder to, configure, or depend on any Plus rear pad. `TP_COMMIT` and
`TP_SNAPSHOT` are logic-analyzer evidence, not product inputs.

## Exact bill of materials

### Active boards

| Qty | Part | Exact identifier | Source-backed dimensions and mass |
| ---: | --- | --- | --- |
| 1 | Seeed Studio XIAO nRF52840 Plus | Seeed SKU 102010672 | 21 x 17.8 mm; authoritative board mass not found, so weigh the received board [A1] |
| 1 | Adafruit TCA9548A I2C multiplexer | Adafruit product 2717 | 30.6 x 17.6 x 2.7 mm; 1.8 g [A2] |
| 6 | Adafruit LSM6DS3TR-C breakout | Adafruit product 4503 | 25.3 x 17.8 x 4.6 mm; 1.7 g each [A4] |
| 1 | Half-size plated breadboard PCB | Adafruit Perma-Proto product 1609 | 81 x 51 x 1.6 mm; 12 g; 1.2 mm plated holes [A11] |

The known board mass before the XIAO, connectors, wire, fasteners, and carriers
is 24.0 g: 12 g + 1.8 g + 6 x 1.7 g. This is arithmetic, not a measured worn
mass.

### Six detachable sensor branches

| Qty | Part | Exact manufacturer identifier | Frozen use |
| ---: | --- | --- | --- |
| 6 | Five-circuit positive-latch cable housing | Molex SL 50-57-9405 / 0050579405 | One at the controller end of each sensor pigtail [A7] |
| 6 | Five-circuit shrouded vertical PCB header | Molex SL 70543-0004 / 0705430004 | One on the fanout for each mux channel [A9] |
| 30 usable plus at least 10 spare | Female crimp contact | Molex SL 16-02-0082 / 0016020082 | One per conductor; specified for 24-30 AWG and 1.52 mm maximum insulation diameter [A8] |
| 1 or qualified crimp service | Hand crimp tool | Molex 200218-7000 | SL 24-30 AWG production crimp; do not substitute pliers [A10] |
| 1 spool of each color | 26 AWG stranded copper hook-up wire | Pololu 2622 red, 2620 black, 2624 yellow, 2626 blue, 2629 white | Each listed spool is 70 ft with 1.3 mm outer diameter; reserve at least 1.2 m usable per color for six branches plus rework [A12-A16] |

The SL housing, contact, and header are one mating 2.54 mm SL system. The
70543-0004 is an active, five-circuit, vertical, shrouded through-hole header
with a 3.30 mm tail [A9]. Its 2.54 mm pin pitch fits the Perma-Proto grid.

The SL header does **not** mount on the sensor breakout. The sensor's required
signals are split across its nine-hole primary row: VIN is JP1 pin 1, GND pin
3, SCL pin 4, SDA pin 5, and INT1 pin 8 in Adafruit's Eagle schematic [A6].
They are not five adjacent holes. Each branch therefore uses individually
soldered wires at the labeled sensor holes, with the five-circuit housing at
the fanout end.

### Mechanical and insulation stock

These are Rev-A engineering choices, not sourced safety ratings:

- five 35 x 25 x 0.8 mm PETG finger carrier plates, all
  corners rounded to 3 mm radius;
- one 135 x 70 x 1.0 mm PETG forearm carrier, all corners rounded to
  5 mm radius;
- one 40 x 32 x 0.8 mm PETG dorsal-reference carrier;
- M2 nylon fasteners and 1 mm nylon spacers for every board that has mounting
  holes;
- one 15 x 15 mm patch of 3M 9088-200 double-coated polyester-carrier tape for
  the XIAO, cut from uncreased stock [A21];
- 1 mm closed-cell EVA skin-side insulation cut to the carrier outline;
- six 12 mm adjustable elastic hook-and-loop straps, one for each finger or
  dorsal carrier;
- two 20 mm adjustable elastic hook-and-loop straps, each initially 300 mm
  long, for the forearm carrier;
- small cable lacing or soft cable ties for strain relief;
- 2 mm heat-shrink for individual sensor-end soldered conductors;
- 6 mm 3:1 heat-shrink for the five-wire bundle behind each SL housing; and
- one 83 x 53 x 0.8 mm PETG fanout solder guard with 3 mm corner radii.

Do not put foam between an IMU board and its rigid carrier. Fasten the board to
the carrier through the nylon spacers, then put the foam between the carrier
and skin. This keeps the board fixed while insulating the wearer.

## Frozen connector and wire contract

### Pinout

Use the cavity number in the Molex drawing. Do not infer numbering from a
front, rear, or mating view. Put a permanent red dot beside cavity 1 on every
housing and header before inserting any terminal.

| SL circuit | Color | Fanout source | Sensor hole | Meaning |
| ---: | --- | --- | --- | --- |
| 1 | Red | XIAO 3V3 rail | VIN | 3.3 V power into the breakout regulator |
| 2 | Black | Common GND | GND | Power and logic return |
| 3 | Yellow | TCA SC0-SC5 | SCL | Separately selected downstream I2C clock |
| 4 | Blue | TCA SD0-SD5 | SDA | Separately selected downstream I2C data |
| 5 | White | XIAO D0/D1/D2/D3/D6/D7 | INT1 | Dedicated data-ready signal |

Adafruit explicitly documents 3 V from a 3 V controller to sensor VIN, and the
standard red/black/yellow/blue colors for VIN/GND/SCL/SDA [A5]. The white INT1
assignment is the Rev-A addition. Leave sensor 3Vo, D0, CS, INT2, SCX, and SDX
unconnected. Leave every AD0 jumper open so each sensor remains at 0x6A.

### Length and routing

Measure finished conductor length from the sensor solder pad to the rear of its
crimp contact. All five conductors in one branch must meet the target below to
within +0/-2 mm. These are frozen construction lengths, not estimates to trim
during fitting.

| Header | Mount | Finished conductor length |
| --- | --- | ---: |
| J0 | Thumb | 145 mm |
| J1 | Index | 150 mm |
| J2 | Middle | 150 mm |
| J3 | Ring | 150 mm |
| J4 | Pinky | 150 mm |
| J5 | Dorsal hand | 105 mm |

- Keep all five conductors in one bundle. Do not separate SCL/SDA from their
  ground return except for the final 15 mm at either end.
- Record all five finished lengths. Do not coil excess wire over an IMU.
- Route the bundle along the dorsal side of the finger, form one relaxed service
  loop at the proximal joint, and then route toward the forearm carrier.
- The 150 mm ceiling is a provisional signal-integrity choice. If a wearer
  cannot move without tension at that length, stop. Move the fanout or define a
  measured new harness revision; do not silently lengthen one branch.

### Crimp and strain relief

1. Cut all five conductors for one branch and label both ends with hand, sensor
   index, and mount name.
2. Strip and crimp the fanout end to the terminal manufacturer's specification.
   Do not tin a conductor before crimping.
3. Inspect conductor and insulation crimps under magnification. Reject a folded
   strand, exposed strand outside the conductor barrel, pierced insulation, or
   asymmetric crimp.
4. Perform and record a pull test on one sacrificial crimp from each wire spool
   before building production branches. Do not invent a numeric pass force
   without the applicable Molex crimp specification and a calibrated gauge.
5. Insert contacts into the housing in the frozen order and tug each wire to
   confirm the retention lance engaged.
6. At the sensor end, pass each conductor through its heat-shrink, solder it to
   the labeled 0.100-inch hole, inspect it, and shrink the sleeve without heating
   the IMU package.
7. Anchor the bundle to the carrier within 10 mm of the sensor board. Pull on
   the cable beyond the anchor and verify no load reaches a soldered hole.
8. Shrink the 6 mm sleeve behind the housing over the bundle, not over the
   latch. The latch must remain visible and operable.

## Fanout assembly

### Connector placement

Use the Perma-Proto only as a plated fanout and board carrier. Its pads are
connected like a breadboard in five-hole groups [A11]. A five-pin header placed
along one connected group would short all five circuits.

Orient the silkscreen upright with row 1 at the hand-facing end and row 30 at
the USB-facing end. Mount every SL header across numbered rows, never along one
connected A-E or F-J group. Freeze these exact pin positions:

| Header | Perma-Proto holes, circuits 1 through 5 |
| --- | --- |
| J0 | C1, C2, C3, C4, C5 |
| J1 | C8, C9, C10, C11, C12 |
| J2 | C15, C16, C17, C18, C19 |
| J3 | H1, H2, H3, H4, H5 |
| J4 | H8, H9, H10, H11, H12 |
| J5 | H15, H16, H17, H18, H19 |

This uses two isolated breadboard halves and leaves two empty numbered rows
between connector pin fields. Circuit 1 is always the lower-numbered row. Add
the red pin-1 mark before soldering. Dry-fit all six header bodies and latches;
stop if any body overlaps or blocks its neighbor. Before soldering a header,
use a meter to prove its five target holes are mutually isolated and that no
same-numbered circuit is tied to another header.

The exact channel labels are:

| Header | Mux channel | Sensor index | Mount | INT1 GPIO |
| --- | ---: | ---: | --- | --- |
| J0 | 0 | 0 | Thumb proximal phalanx | D0 |
| J1 | 1 | 1 | Index proximal phalanx | D1 |
| J2 | 2 | 2 | Middle proximal phalanx | D2 |
| J3 | 3 | 3 | Ring proximal phalanx | D3 |
| J4 | 4 | 4 | Pinky proximal phalanx | D6 |
| J5 | 5 | 5 | Dorsal hand | D7 |

Mark each header on both the silkscreen side and solder side. A connector that
can be plugged into the wrong channel without the labels being obvious fails
assembly inspection.

Route XIAO D8/P1.13 to Perma-Proto C23 and fit a labeled analyzer test pin at
A23 in the same connected row. Name that net `TP_SNAPSHOT`. Prove continuity
from D8 to A23 and isolation from every branch signal, rail, and adjacent row.
Route XIAO D9/P1.14 to C24 and fit a second labeled analyzer test pin at A24.
Name that net `TP_COMMIT`. Prove continuity from D9 to A24 and isolation from
every branch signal, rail, D8, and adjacent row. Both test pins must be
accessible on the guarded bench rig but must not face or touch skin.

### Power, pull-ups, and capacitors

Use the red and blue rails beside the A-E half as fanout positive and negative,
respectively; leave the two F-J-side rails unconnected. Wire controller GND to
the negative rail, the TCA GND, and circuit 2 of J0-J5. Wire the positive rail
to TCA VIN and circuit 1 of J0-J5. Name the single
controller-3V3-to-positive-rail bridge `PWR_LINK`. Leave `PWR_LINK` open through
the isolated-rail measurements in Gate B. Install a red 26 AWG link with a
finished conductor length of 30 +0/-2 mm only after Gate B's split-rail
measurements pass. Seeed's official Plus v1.1 KiCad source identifies
U6 as a 3.3 V TLV733P, and TI rates that regulator for 300 mA [A22][A23]. The
component rating does not state the current guaranteed at `3V3_OUT` after the
board's own demand, voltage drop, and thermal limits. Gate B therefore qualifies
a conservative experimental total-input envelope on the received board, not a
manufacturer-guaranteed external-current budget. Keep `PWR_LINK` open until
that procedure permits linking, and keep the qualified current-limited USB
source in the final trial setup. Keep the USB shield out of the signal return.
Do not connect battery pads.

A live fanout rail while the XIAO is unpowered is forbidden. The TCA upstream
pull-ups and powered sensor outputs would otherwise apply voltage to unpowered
nRF52840 pins. Nordic specifies a normal-mode VDD range of 1.7 to 3.6 V, a GPIO
high reference tied to VDD, and an absolute I/O limit of VDD + 0.3 V when VDD is
at most 3.6 V [A19]. Gate B therefore powers the controller first, measures its
live 3V3 rail, and only then enables a separately current-limited fanout supply
set no higher than that measured rail. Power-down removes the fanout supply and
proves the rail discharged before removing controller USB power. No step may
reverse that order.

Adafruit's inspected schematics establish these board-level parts:

- the TCA breakout has 10K upstream SDA/SCL pull-ups through normally closed
  solder jumpers, a 10 uF supply capacitor, and a separate 10K RESET pull-up
  [A3];
- each sensor breakout has 10K level-shifter pull-ups, 10 uF on regulator input,
  10 uF on regulator output, and 0.1 uF at the sensor [A6]; and
- the TCA downstream channels have no pull-ups, while each selected sensor
  branch supplies its own onboard pull-ups [A2][A5].

The TCA channels are bidirectional translating switches [A17]. Selecting a
branch connects it to the upstream pair and deselecting it disconnects that
path. This is branch selection, not galvanic isolation.

Therefore Rev A adds no I2C pull-up and no bulk or local bypass capacitor. Do
not remove or bridge the boards' pull-up jumpers. If measured edges or rail
droop fail, record the trace and define one component-valued variant. Do not
add an undocumented capacitor or resistor to make a run pass.

Leave TCA RST electrically open at the fanout. Its verified onboard 10K pull-up
holds reset inactive [A3]. Rev A has no software-controlled mux reset and no
reset conductor. The separate D8 `TP_SNAPSHOT` and D9 `TP_COMMIT` conductors
are mandatory Gate 0A instrumentation. A later controlled-reset variant must
name an accessible pin, add it to the wiring manifest, verify it by continuity
and logic analyzer, and rerun all Gate 0A evidence. It is not Rev A.

## Mechanical placement

### Dimensioned carrier template

Use these dimensions as the fabrication drawing. All dimensions and coordinates
are millimeters and refer to the electronics-side view. Cut outlines to +/-0.5
mm, holes and slots to +/-0.25 mm, and board reference rectangles to +/-0.25
mm. Print or plot with scaling disabled and include a 100 mm gauge line; reject
the template if that line measures outside 99.5 to 100.5 mm. Round every slot
end. Deburr every edge before installing electronics or straps.

The lowercase `x` and `y` below are drawing coordinates, not characterized
sensor axes. For a finger or dorsal carrier, the origin is its proximal-ulnar
corner, drawing x increases distally, and drawing y increases radially. For the
forearm carrier, the origin is the distal-ulnar corner, drawing x increases
proximally, and drawing y increases radially. A rectangle is specified as
`x minimum, y minimum, width, height`. A slot's long dimension follows the
larger size.

The five identical finger carriers use this schedule:

| Feature | Exact geometry |
| --- | --- |
| Carrier outline | 0, 0, 35, 25 with 3 mm corner radius |
| IMU reference rectangle | 4.85, 3.60, 25.30, 17.80 |
| Proximal strap slot | center 2.25, 12.50; 1.50 x 13.00 |
| Distal strap slot | center 32.75, 12.50; 1.50 x 13.00 |
| Proximal cable-lace holes | 1.8 mm diameter at centers 3.90, 9.75 and 3.90, 15.25 |

Put the IMU's 25.3 mm edge along drawing x, its component side up, and its
nine-hole primary header edge on the radial side of the reference rectangle. Clamp the
received breakout exactly to that rectangle and transfer-punch its actual
mounting-hole centers into the carrier. Drill those transferred centers to 2.2
mm. The received vendor board is the drilling jig because no source-backed
mounting-hole coordinates were established. Do not estimate or freehand those
centers. Use both vendor mounting holes, M2 nylon fasteners, and 1 mm spacers.

The dorsal carrier uses this schedule:

| Feature | Exact geometry |
| --- | --- |
| Carrier outline | 0, 0, 40, 32 with 3 mm corner radius |
| IMU reference rectangle | 7.35, 7.10, 25.30, 17.80 |
| Ulnar strap slot | center 20.00, 3.00; 13.00 x 1.50 |
| Radial strap slot | center 20.00, 29.00; 13.00 x 1.50 |
| Proximal cable-lace holes | 1.8 mm diameter at centers 5.50, 13.25 and 5.50, 18.75 |

Use the same IMU orientation and transfer-drill procedure. The two 13 mm slots
carry one 12 mm strap around the hand. The strap and carrier must stay clear of
the palm contact area and wrist crease in full motion; failure selects a new
mechanical revision rather than an improvised strap path.

The forearm carrier uses this schedule:

| Feature | Exact geometry |
| --- | --- |
| Carrier outline | 0, 0, 135, 70 with 5 mm corner radius |
| Fanout solder-guard reference rectangle | 6.00, 8.50, 83.00, 53.00 |
| Perma-Proto reference rectangle | 7.00, 9.50, 81.00, 51.00 |
| XIAO reference rectangle | 91.00, 10.00, 21.00, 17.80 |
| XIAO tape patch | 94.00, 11.40, 15.00, 15.00 |
| TCA reference rectangle | 92.00, 42.00, 30.60, 17.60 |
| Distal strap slots | centers 25.00, 3.00 and 25.00, 67.00; each 21.00 x 2.00 |
| Proximal strap slots | centers 110.00, 3.00 and 110.00, 67.00; each 21.00 x 2.00 |

Put the solder guard flat in its reference rectangle. Align the Perma-Proto
above it, transfer the board mounting holes through both PETG layers, and hold
the Perma-Proto 1 mm above the guard with its nylon spacers. Reject any clipped
lead that touches or pierces the guard. Put Perma-Proto row 1 at x = 7.00 and
row 30 toward drawing +x. Put the XIAO USB
receptacle toward +X and route its cable through the open carrier area from x =
112 to x = 135. Orient the TCA so its upstream VIN/GND/SDA/SCL edge faces the
XIAO and all channel labels remain readable. Align the Perma-Proto and TCA to
their reference rectangles, transfer-punch their actual mounting holes, and
drill 2.2 mm as above. Use every available mounting hole with M2 nylon hardware
and 1 mm spacers. The XIAO has no frozen mounting-hole pattern. Clean and dry
both bonding surfaces, apply only the exact tape patch in the table, press the
XIAO component-side up into its reference rectangle, and leave it unloaded for
24 hours before wiring. Reject any yaw or edge lift; do not add glue or a second
fastening path. The strip from x = 88 to x = 91 is the fanout wire
crossing; no board or strap may occupy it. Route `PWR_LINK` directly through
this crossing and finish it at 30 +0/-2 mm. No electronics, fastener, or clipped
lead may extend outside its reference rectangle except the USB receptacle and
insulated wires.

Before soldering, place the bare boards and straps on the cut carriers, take a
straight-on photograph with a steel rule, and overlay the coordinate schedule.
Any feature outside tolerance stops fabrication. This schedule fixes the Rev-A
construction; wearer fit remains a hardware gate and may reject it.

### Finger sensors

Mount all five finger boards only on the dorsal side of the proximal phalanx.
Do not mount on the middle phalanx, fingertip, palm side, or across a joint.

For every finger:

- place the component side away from skin;
- align the board's 25.3 mm dimension with the proximal-distal finger direction;
- put the breakout's nine-hole primary header edge on the radial side and turn
  the strain-relieved bundle toward the wrist without a sharp bend;
- mark `+X distal` and `+Z dorsal`; mark `+Y radial` for a right-hand carrier or
  `+Y ulnar` for a left-hand carrier so the carrier frame remains right-handed,
  independent of the sensor's raw axis marks; and
- tighten only until the carrier no longer yaws during a light hand open/close.

At the first fitting, elastic extension from its slack circumference must not
exceed 5 percent. Record slack circumference, worn circumference, strap overlap,
and board-plus-carrier mass for every mount. Five percent is a conservative
Rev-A starting limit, not a medical pressure rating. Stop if the carrier still
moves at that limit rather than tightening further.

The board may not touch the contact surface or an adjacent finger during full
comfortable flexion and extension. If the 25.3 mm breakout cannot meet that
rule on a finger, that finger fails the breakout mount. Do not move it to a
different phalanx to preserve the build.

### Dorsal reference

Fasten the sixth sensor to the 40 x 32 mm carrier on the back of the hand,
between the metacarpals and clear of the wrist crease. Use the same component-
side-out rule, align its 25.3 mm dimension distal, and put its nine-hole primary
header edge radial. Use the same carrier reference arrows. The dorsal board
must move with the hand, not with the forearm carrier or a loose cable.

### Controller and fanout

Place the XIAO, TCA, and Perma-Proto on the 135 x 70 mm carrier on the dorsal
distal forearm. The carrier must end at least 15 mm proximal to the wrist crease
when the hand is fully extended. Put the USB connector at the proximal end so
the host cable routes up the forearm, away from the hand.

Fasten the Perma-Proto and TCA to the rigid side with the specified nylon
hardware. Bond the XIAO only with the dimensioned tape patch.
No solder side or clipped lead may face skin. Cover the entire solder side with
the nonconductive guard, then put the 1 mm EVA layer between carrier and skin.
Use two 20 mm straps through separate carrier slots so one loose strap cannot
rotate the electronics onto the skin.

At the first fitting, neither forearm strap may exceed 5 percent extension from
its slack length. Leave at least 50 mm of hook-and-loop overlap. Record final
carrier mass and strap geometry. Route USB through a separate strain-relief
loop on the forearm; the USB connector must not carry cable pull.

Stop wearing immediately for pain, numbness, tingling, blanching, color change,
skin marking that persists after removal, warmth, restricted circulation, or
restricted joint motion. Removal must require no tool and take less than five
seconds in a timed bench rehearsal before anyone wears the powered rig.

## Assembly sequence

1. Photograph and record every board identifier, revision marking, and visible
   serial or lot mark. Weigh each board as received.
2. Fabricate the five finger, one dorsal, and one forearm carrier from the
   dimensioned schedule. Dry-fit boards and straps and reject any feature
   outside its stated tolerance.
3. Mark J0-J5, pin 1, and the channel/mount table on the fanout before soldering.
4. Place the six SL headers across isolated Perma-Proto rows. Prove isolation,
   then solder and recheck it.
5. Mount or wire the XIAO and TCA. Leave RST, all rear pads, battery pads, and
   channels 6-7 unconnected.
6. Wire GND, the fanout positive rail, upstream D4/D5, the six downstream SC/SD
   pairs, the six dedicated interrupt GPIOs, D8 to `TP_SNAPSHOT`, and D9 to
   `TP_COMMIT`. Label and leave `PWR_LINK` open until Gate B directs its
   installation.
7. Build one sensor pigtail at a time. Complete its exact finished length,
   crimp, solder, strain-relief,
   and point-to-point continuity record before starting the next.
8. With all boards disconnected, verify every fanout net against the frozen
   table and verify no adjacent connector circuits are shorted.
9. Connect one unpowered sensor at a time and verify that no signal, power, or
   ground appears on the wrong board hole. Check connector latch retention.
10. Mount boards to carriers and add the guard and skin insulation. Repeat the
   continuity check after all fasteners are tight.
11. Create the first as-built manifest and photo hashes before applying power.

## Axis-map and mount-provenance procedure

The decoder must not assume that board silk axes equal carrier axes. Axis work
has two separate stages: a non-authorizing physical characterization, then an
operational manifest binding. A report that merely contains a plausible matrix
does not authorize acquisition.

Use the exact coordinate convention
`x-distal-z-dorsal-right-handed-y-by-hand-v1`:

```text
+X = distal, toward the fingertip
+Z = dorsal, away from the palm
+Y = the direction that makes X cross Y equal Z
     right hand: radial, toward the thumb
     left hand: ulnar, toward the pinky
```

This is a right-handed frame for both hands. Each characterization artifact
must state `hand` and its required `positive_y_direction`; a right-hand report
cannot be reused on a left hand.

### Stage 1: characterization artifact

Before freezing the rig record, complete all rig checks, including the
standalone analyzer preflight. That preflight is not the later full Gate 0A
capture. Do not mark unperformed checks passed. Photograph the component face,
carrier arrows, sensor index, strain relief, and connector pin-1 mark in one
frame, then take a second photo after mounting. Hash the original photo bytes
and bind them through the rig record. Any subsequent rig-record change requires
a new characterization.

Start from `configs/acquisition-template.example.json`. Replace its declared
builds, local participant alias, and as-built identities with the actual values.
Keep the pending zero boot IDs, provisional identity matrices, and example axis
report hash until commissioning replaces the measured fields. Those placeholders
are not approved evidence and cannot authorize an operational recorder.

Run `hash-rig-evidence <rig-evidence>` and put its lowercase SHA-256 output in
the candidate template's `rig_evidence_sha256`. This validates and hashes the
typed canonical record, not its whitespace or input key order. The command
prints only the hash on stdout and exits nonzero for an invalid record. Hashing
an incomplete record does not make its checks pass or authorize commissioning.

1. Freeze the as-built `invisible-keyboard-rig-evidence-v1` record and the
   pre-capture acquisition template identities. Use the implementation's
   canonical candidate projection, not the source JSON bytes, to compute
   `candidate_manifest_sha256`. This candidate intentionally excludes boot IDs,
   selected axis maps, and later calibration results.
2. Put each complete board-plus-carrier in a right-angle fixture whose faces
   are labeled `+X up`, `-X up`, `+Y up`, `-Y up`, `+Z up`, and `-Z up` under
   the stated hand convention.
3. Run `characterize-axes <template> <rig-evidence> <output-directory>
   <device> [<device>]` on Linux, with devices in template hand order. It needs
   a terminal for pose acknowledgements and never creates an OS input device.
   It validates the template and exact rig binding without loading an axis
   report.
   The command prompts in hand order, sensor index order, then the six face
   order above. Press Enter only after positioning the carrier. After one
   second of settling it records exactly 600 consecutive frames for that hand.
   Each block retains all six sensors, but only the prompted sensor contributes
   to that face mean. Repositioning and settling frames are not in the selected
   source dataset; this is not a continuous acquisition qualification capture.
   The source uses the existing IKS3 format with the canonical candidate hash
   in its header, so it cannot stand in for an operational session.
   Within selected blocks, reject missing/invalid sensors, overrun, transport
   faults, changed identity, clock regression, sequence gaps, clipped target readings, target gyro
   components over 100 LSB in absolute value, or target acceleration component
   ranges over 820 LSB. A face must span at least 2.9 device-clock seconds with
   strictly advancing target sample timestamps. Reject the clock-unsynced flag:
   it denotes a controller-clock fault, not a pending host-clock fit. This
   characterization does not qualify host/device clock alignment.
   Abort on Ctrl-C, device disconnect, or the 1,200-second total limit; keep
   partial raw evidence but publish no axis report.
4. Replay the selected session in the same fixed block order and compute each
   face's mean raw accelerometer vector using integer division toward zero.
   Require exactly 600 frames per block and no extra records. Select one unique,
   determinant-positive signed permutation from raw sensor axes to the carrier
   frame and apply the same matrix to accelerometer and gyro vectors. Reject
   ambiguity or a reflection instead of guessing.
5. Emit an `invisible-keyboard-axis-characterization-v1` report. Use
   `expected_gravity_lsb = 8192`,
   `maximum_component_residual_lsb = 820`, and
   `minimum_samples_per_face = 600`. For every sensor, store the six raw means,
   sample counts, selected `axis_map`, recomputed maximum component residual,
   and pass result. The implementation must recompute residuals and every pass
   bit from those raw means. A failed sensor makes the whole report fail.
6. The report also stores its report ID, completion time, characterizer build,
   method ID `fixed-order-600-stationary-iks3-v1`, exact coordinate convention,
   candidate manifest hash, source
   session hash, and exact rig-evidence hash. Hash the implementation's compact
   canonical report JSON. A raw `sha256sum` of differently formatted JSON is not
   a substitute for that canonical identity.

Operational admission accepts only this method and these exact criteria, with
exactly 600 samples in every face. A report cannot select its own pass limits
or authorize an older measurement method by changing and rehashing its fields.

The command creates a new directory containing `axis-session.iks`,
`axis-report.json`, and `acquisition-template.json`; it never overwrites an
existing directory. The final template contains the derived matrices and report
hash but retains pending boot IDs. `derive-axes <template> <rig-evidence>
<axis-session> <output-directory>` runs the same reducer without devices and
publishes a report and bound template in a new directory. Report identity uses
the source session ID; completion time and `characterizer_build` identify the
reduction run. Replaying the same session reproduces the numerical evidence,
not a later run's timestamp or executable identity. Both commands print
artifact paths on stdout, diagnostics on stderr, and exit nonzero on failure.
Neither command authorizes calibration, normal acquisition, or live output.

The 820 LSB residual is a Rev-A engineering threshold. It is not a sensor
accuracy claim or proof of key recognition. Hardware data may reject it and
select a new declared revision; do not edit a failed report to pass.

### Stage 2: operational manifest binding

1. Use the generated `acquisition-template.json`. It already carries every
   derived sensor matrix, the report's canonical `axis_characterization_sha256`,
   and the frozen canonical `rig_evidence_sha256`. Do not hand-edit measured
   fields to make a rejected template pass.
2. Load the template together with the exact rig-evidence and axis-report bytes
   through the evidence-binding manifest loader. Structural manifest validation
   alone is not admission.
3. Require exact matches for hand, canonical sensor index and role, mux channel,
   mount ID, wiring ID, interrupt pin, controller identity, rig hash, candidate
   hash, report hash, and every axis-map element. Lock controller-generated boot
   IDs only after those checks pass.
4. Apply the bound matrix exactly once after wire-frame identity validation and
   before neutral qualification, fitting, held-out evaluation, replay, or live
   decoding. Firmware and stored raw frames remain in sensor axes.
5. A changed carrier, loosened board, reversed connector, moved or swapped
   branch, new mount ID, new wiring ID, or changed acquisition profile creates a
   new candidate and repeats both stages. Never copy an old matrix into a new
   manifest by inspection.

## Unpowered and powered bring-up gates

### Gate A: unpowered harness

Pass only when:

- all 30 connector circuits have point-to-point continuity;
- no unintended low-resistance path exists; shared power/ground and onboard
  pull-up paths match the netlist and inspected schematics;
- J0-J5 map to the declared sensor and GPIO;
- every latch retains under a gentle cable pull at the strain relief;
- branch length, wire color, and both end labels match the manifest; and
- articulating the unpowered carriers and service loops on a hand fixture puts
  no tension on a sensor solder joint.

Any polarity error, intermittent circuit, cross-channel connection, damaged
crimp, or wire-driven board movement is a stop. Rework it and repeat the full
unpowered gate.

### Gate B: current-limited bench power

Do this on a nonconductive bench, not on a person. Use one DMM for all voltage
comparisons so meter offset does not look like rail mismatch. `PWR_LINK` stays
open throughout the split-rail characterization.

Required equipment: two independently current-limited bench outputs, a DMM,
a current probe or calibrated shunt with differential measurement, an analog
oscilloscope with at least 1 MHz measurement bandwidth and 10 MS/s sampling,
and contact temperature probes. A logic analyzer cannot measure rail or current
overshoot. Use a USB data/power breakout with host VBUS physically disconnected:
only D+, D-, and GND pass through to the host; the first bench output alone
supplies device VBUS. Meter-prove this separation unpowered, including every
hub and analyzer connection. No battery, rear-pad supply, or second VBUS feed
is permitted. These are required lab resources, not included wearable BOM
parts; obtain a lab quote before scheduling this gate.

Set VBUS to 5.0 V with a fixed 80 mA current limit. Before connecting the rig,
qualify the source and exact cable/breakout with a dummy load: record startup,
a 40-to-100 mA requested load step, and a brief short. Require VBUS never above
5.25 V, current no higher than 100 mA in the recorded trace, and current at or
below 80 mA within 1 ms of overload. Include instrument error in these bounds;
retain the traces and instrument settings. This is a finite-bandwidth
engineering check, not an assertion that faster transients cannot occur.
Do not increase the limits to pass. An unqualified source blocks this gate.

The normal-operation acceptance limit is 64 mA total input, leaving 25 percent
headroom to 80 mA. With no other power source, total USB current conservatively
bounds the regulator's sustained load without subtracting an unknown board
load. At VBUS 5.25 V, rail 3.25 V, and 80 mA, the pass-element dissipation bound
is 0.16 W using TI's `(VIN - VOUT) * IOUT` relation [A23, section 8.1.3]. This
is not a board thermal guarantee: TI explicitly says actual board thermal
impedance varies. The linked thermal test below remains mandatory. These
limits are Rev-A engineering choices for supervised trials, not vendor ratings
or safety certification.

1. Disconnect every sensor, USB, and the bench supply. Inspect and meter-prove
   that `PWR_LINK` is open. Connect the bench-supply return to controller GND,
   but leave its positive output disconnected and off.
2. Flash the hand-specific verified production UF2 using the firmware handoff
   before this gate. Hold the controller's reset button down during partial
   population measurements, including power-up. Power the XIAO from the
   qualified current-limited USB source first. Record the
   controller-only steady and maximum 60-second USB current. Measure `V_CTRL`
   from the XIAO 3V3 edge pin to controller GND twice, ten seconds apart. Stop
   if the readings differ by more than 10 mV or the rail is outside the limits
   applicable to both the XIAO and connected breakouts, or outside 3.25-3.35 V.
3. With the bench supply still disconnected from the fanout, set its unloaded
   output to a value from `V_CTRL - 50 mV` through `V_CTRL`, never above
   `V_CTRL`. Verify both the supply readback and the output terminals with the
   DMM. Stop if that match cannot be held or if the selected value is outside a
   breakout's documented input range. Turn the output off and return its set
   voltage to 0 V before connecting its positive lead to the fanout rail.
4. With only the TCA attached to the fanout rail, set a 5 mA limit, enable the
   supply at 0 V, and ramp no faster than 0.1 V/s to 1.0 V. Hold for ten
   seconds. Raise the limit to 10 mA and ramp to 2.0 V. Hold for ten seconds.
   Raise the limit to 20 mA and ramp to the matched voltage from step 3. Do not
   raise a limit to make a stage pass. Stop on current-limit operation, rail
   collapse, odor, visible damage, or perceptible warmth.
5. Record voltage at the source terminals, fanout rail, TCA VIN, XIAO 3V3, D4,
   and D5. The fanout rail must not exceed `V_CTRL`; the source-to-fanout drop
   and `V_CTRL`-to-fanout difference must each be at most 50 mV. These are
   Rev-A engineering limits, not vendor guarantees.
6. Add J0 through J5 one at a time. Before each connection, turn the fanout
   output off, return its set voltage to 0 V, and verify the fanout rail is
   below 0.10 V while leaving the XIAO powered by USB. Connect only the next
   branch. Set the next current limit to the prior stable current plus 10 mA,
   with an absolute ceiling of 80 mA, then repeat the 0.1 V/s ramp to the
   matched voltage. Stop if the limit ceiling is reached before all branches
   run. Do not unplug USB during this loop.
7. At every population count, record 60 seconds of unconfigured standby
   current, source voltage, fanout voltage, and voltage at the newly added
   sensor VIN. Keep reset asserted: production firmware requires all six
   sensors and cannot provide configured acquisition for a partial bank.
   Only after all six branches have stable power, release reset once and
   record 60 seconds of configured acquisition with that same production
   firmware. Record simultaneous controller USB and fanout currents. Their
   sum must remain at or below 64 mA, including measurement error, throughout
   this configured interval. Record the current instrument's bandwidth. Do not
   call an unobserved faster transient bounded. Reassert reset before shutdown.
8. For shutdown, stop acquisition, turn the fanout output off, return its set
   voltage to 0 V, and prove the fanout rail, D4, D5, and all populated INT1
   lines are below 0.10 V. Disconnect the bench positive lead. Only then
   disconnect USB. A fanout-first power-up or USB-first power-down invalidates
   the run.
9. Bind the receipt to the received Plus v1.1 board and its photographed U6
   marking matching the official schematic [A22]. A different revision or
   regulator requires a new electrical design review, not an assumed match.
   Record both source settings, waveform receipts, firmware digest, all rail
   readings, and simultaneous full-bank current measurements. If any
   prerequisite or measurement fails, leave `PWR_LINK` open and stop.
10. With every source disconnected and both rails below 0.10 V, install
    `PWR_LINK`, prove its continuity and isolation from GND, and remove the
    bench fanout lead completely.
11. Power the linked rig through the same qualified USB path at the unchanged
    80 mA limit. Capture boot and reset waveforms. In normal operation require
    VBUS 4.75-5.25 V, controller 3V3 3.25-3.35 V, each branch VIN within 50 mV
    of controller 3V3, and total input at or below 64 mA including instrument
    error. Require stable enumeration and continuous full-bank acquisition.
    Stop on current-limit operation, rail collapse, odor, visible damage, or
    acquisition failure. Do not increase a limit to make the board work.
12. At ambient 20-25 degrees C, run configured acquisition for 20 minutes on
    the fully assembled, insulated carrier off-body. Measure U6, the USB
    connector, sensor boards, and every skin-facing surface. Require all
    measured surfaces below 38 degrees C including probe error, and less than
    1 degree C rise in the last five minutes. Stop immediately if any limit
    is reached. Retain the temperature series with the electrical receipt.
    Only then may Gate B pass. Keep this exact USB source, current limit,
    cable/breakout, battery-free configuration, and radio-disabled firmware
    throughout subsequent supervised trials; ordinary direct-host USB power
    is not qualified. Any power-path change requires Gate B again.

### Gate C: firmware and channel identity

Use the hand-specific, digest-verified Bazel UF2 and immutable flash record from
the selected-board firmware handoff. Its Rev-A overlay must use only D0 through
D9. It must contain no mux-reset GPIO. It must bind the dedicated
active-high `ikb-snapshot` output to D8/P1.13 and `ikb-cache-commit` output to
D9/P1.14, initialize both physically low, and fail boot if either output cannot
be configured.

Arm the analyzer before a full sensor/controller power cycle, or a controller
reset after verified sensor quiescence. A controller-only reset of streaming
sensors does not satisfy the quiet-anchor precondition. Before sensor I/O or
interrupt enable, firmware must emit `D8 high >=20 us, both low >=20 us, D9
high >=20 us, both low >=20 us`. This is the boot/sequence anchor, not a normal
cache or snapshot event. Missing, repeated, reordered, overlapping, or
sensor-active anchor phases fail Gate 0A.

The [startup admission contract](../../firmware/zephyr/README.md#startup-admission)
requires all six quiet profiles, installed GPIO callbacks, serialized
per-sensor arming/readback, and bounded clean-cache priming. The analyzer must
validate that sequence before accepting normal snapshots; configuration
traffic is not an acquisition read.

Pass only when:

- USB CDC enumerates repeatedly without enabling radio or battery behavior;
- the TCA answers at 0x70, the selected downstream branch is connected, and
  deselected branches are disconnected;
- WHO_AM_I returns 0x6A on channels 0-5 and no sensor appears on 6-7;
- each physical INT1 produces edges only on its declared GPIO;
- disconnecting one branch invalidates that sensor without changing identities;
- every permanent housing and branch label matches the continuity record and
  mounted photo. The six identical IMUs expose no per-board machine identity,
  so a physical swap cannot be detected from `WHO_AM_I` or the static manifest;
- any disconnected, moved, or swapped branch receives a new mount ID and
  repeats continuity inspection, mounted photos, the characterization artifact,
  and operational manifest binding;
- firmware writes and reads back `DRDY_PULSE_CFG_G` as `0x80`, and the analyzer
  observes distinct approximately 75 us INT1 pulses as specified by the
  LSM6DS3TR-C datasheet [A18];
- one DSLogic Plus uses raw bits 0 through 5 for sensor INT1, bit 6 for D8, bit
  7 for D9, bit 8 for SDA, and bit 9 for SCL. It confirms 400 kHz upstream I2C,
  mux select/readback, the exact status/motion direct-register sequence, a real
  six-pending collision, and every completed read finishing before that
  sensor's next INT1 edge;
- the analyzer sees the complete boot anchor before any INT1 or I2C activity,
  then the exact configuration and arming transfers. The first normal D8
  snapshot maps to sequence 0 and normal D8 ordinal N maps to frame sequence N;
- the full capture reports pre-admission pulses separately and accounts for
  every admitted INT1 pulse, corresponding read or pending-read collision,
  D9 cache commit, and D8 snapshot. Every completed motion read is selected
  or deliberately decimated. Pre-first-D8 decimation remains explicit rather
  than receiving an invented phase-histogram bin;
- after I2C completes, each worker locks cache then sensor state, raises D9,
  stores and latches one slot, holds D9 for at least 2 us, attempts D9 low, and
  clears pending while the sensor-state lock remains held. Successful D9 fall
  is cache eligibility. Any same-sensor physical INT1 before that fall
  invalidates the capture;
- at every 200 Hz boundary, main locks the same cache, raises D8, copies and
  consumes all six slots, holds D8 for at least 2 us, lowers it, and unlocks.
  D8 rise is the aggregate eligibility boundary and the copied array remains
  immutable during encoding. Each selected sensor value is its newest
  unambiguous eligible read before that boundary;
- every normal D8 and D9 high interval is at least 2 us and less than 20 us,
  and the two normal marker intervals never overlap;
- a valid `IKQ2` request closes admission on one clean scheduled final D8
  snapshot only when all six caches are fresh and every admission, work,
  commit, timer, and terminal-transport prerequisite is idle or reserved;
- after final D8 falls, the bus shows exactly 52 ordered shutdown transactions
  and 1,422 SCL rises: initial mux deselect/readback, four write/readback pairs
  for sensors 0 through 5 in order, then final mux deselect/readback;
- after 200 us of idle-high SDA/SCL and low INT1, D8 and D9 rise together in
  one GPIO1 port operation, stay high for 200 through 400 analyzer samples,
  fall together, and never change again. No `0x0340` or `0x0380` intermediate
  sample is permitted between idle `0x0300`, marker `0x03c0`, and terminal
  `0x0300`;
- the controller offers one matching `IKR2` receipt only after marker fall,
  emits no later application byte, and the raw capture retains at least
  10,000,000 idle samples after the marker;
- selected samples retain their source-edge timestamps, expected nominal
  208-to-200 decimation approaches 8 reads per second per sensor, and expected
  decimation does not set acquisition overrun; and
- the report does not claim that direct output-register reads captured every
  internal conversion unless the analyzer's pulsed-INT1 evidence establishes
  it;
- a passed axis-characterization report and the operational manifest bind all
  six fixed carriers exactly; and
- USB power cycle creates a new boot identity and resumes at a COBS delimiter.

The source aggregate timer and sequence advance even when DTR is absent. The
first normal D8 maps to sequence 0, each later D8 advances once, and reconnect
never resets the sequence. The host does not monitor DTR as a fault signal. A
host-visible source disconnect or transport fault still revokes live output,
but D8 does not prove host delivery. The analyzer cannot observe USB or
host-kernel backlog, DTR, or the kernel read boundary.

The decisive Gate 0A artifact follows the
[DSLogic capture guide](gate0a-capture-stack.md): 7,220,000,000 native
little-endian `u16` words at 2 MHz, 14,440,000,000 bytes, and at least 3,601
scored seconds. The 20 MHz 10-second electrical preflight and 2 MHz 10-minute
soak are non-decisive. Build, finalization, hashing, and scoring run in a
Crabbox; only the physical analyzer capture runs on its compatible host. Follow
[Canonical artifacts](gate0a-capture-stack.md#canonical-artifacts),
[Box validation record](gate0a-capture-stack.md#box-validation-record),
[Physical preflight sequence](gate0a-capture-stack.md#physical-preflight-sequence),
[Physical-host capture](gate0a-capture-stack.md#physical-host-capture),
[Crabbox finalization](gate0a-capture-stack.md#crabbox-finalization), and
[GPL and firmware provenance](gate0a-capture-stack.md#gpl-and-firmware-provenance).
The pinned firmware bytes do not yet reproduce from the available source
snapshots, so redistribution remains a legal and supply-chain risk.

The decisive run uses the single supervising capture wrapper. It owns sigrok
and the Bazel-built `record-hand` process, passes the exact template, rig
evidence, axis characterization, and stable TTY, and coordinates readiness and
stop through private file descriptors. The recorder's operand order is:

```text
record-hand <template> <rig-evidence> <axis-characterization> <device>
  <locked-manifest.partial> <session.partial>
  --ready-fd <fd> --stop-fd <fd>
```

The wrapper requests stop only at raw bytes 14,416,000,000 through
14,417,000,000 and holds the analyzer-success pipe open until exact raw EOF.
The recorder then requires a further 100 ms of TTY silence before attempting
`END3`, sync, and strict audit. Recorder or wrapper stdout is not an oracle.
Any pre-finish protocol, receipt, guard, or signal failure leaves no `END3`.
A later file-sync or post-footer audit I/O failure can leave `END3` bytes in a
hidden partial, but the process exits nonzero and the wrapper publishes
nothing. `END3` bytes alone never prove a successful capture.

TCA recovery in Rev A is a declared whole-controller power cycle or fatal stop.
Software must not pretend it toggled a reset line that is not wired.

### Gate D: non-worn motion rehearsal

Before any person wears the electronics, move each carrier by hand through
ordinary rotations while recording. Flex every harness service loop, disturb
each connector, unplug one sensor, unplug USB, and terminate/restart the host
process. Every fault must remain visible in the evidence and the host output
must stay disabled and all-up. Then execute Gate 0A from the roadmap. Gate 0S
runs later against the two-hand live path and Linux virtual device.

### Gate E: wear admission

Only Gate A-D passes admit a short unpowered fitting. The timed five-second
removal rehearsal, strap-extension records, full joint motion, skin inspection,
and no-adverse-symptom checks must pass before a short powered fitting. A Gate E
pass authorizes evidence collection only. It does not authorize extended wear
or a product safety claim.

## Selection and stop rules

Keep Rev A unchanged through one Gate 0A attempt. Select a new revision instead
of patching the assembly when a measured blocker requires a different branch
length, pull-up, capacitor, power source, reset line, marker net, bus, or mount.

Stop Rev A when any of these occurs:

- one connector can be inserted into the wrong channel without an obvious
  label or the pin-1 convention cannot be inspected;
- a sensor branch needs more than 150 mm to avoid tension;
- the XIAO 3V3-source limit cannot be established or measured rail behavior
  leaves inadequate margin;
- the breakout interferes with an adjacent finger, joint, or contact surface;
- strain relief does not prevent wire force from moving a carrier;
- I2C edges, collisions, next-edge service time, or USB delivery fail Gate 0A;
- an INT1 edge, I2C read, collision, cache commit, snapshot, selection, or
  deliberate decimation is missing or multiply classified in the full native
  raw analyzer capture;
- final D8 does not close one clean scheduled boundary, the 52 shutdown
  transactions or 1,422 SCL rises differ, the terminal D8/D9 transition is not
  simultaneous, or any D0-D9 activity follows it;
- a same-sensor physical INT1 occurs before the successful D9 eligibility fall;
- six-face characterization is ambiguous, fails its declared residual, or no
  longer binds the exact operational mount and wiring identities;
- any electrical fault is silent; or
- any wearer experiences a listed adverse symptom.

Controlled mux reset is a later **measured variant**, not a missing Rev-A wire.
The D9 cache-commit and D8 snapshot markers are mandatory in Rev A. Changing
either boundary requires a new manifest revision and a full Gate 0A rerun. Extra
Plus pads may be considered only after an official mapping, continuity check,
board overlay, and logic-analyzer proof agree.

## As-built record checklist

Store these as each gate runs and complete the set before the first worn data
session. Keep a blocked field explicitly not-run or inconclusive; do not omit
it.

- exact board identifiers and measured board/carrier/harness masses;
- J0-J5 point-to-point continuity table;
- crimp inspection and sacrificial pull-test record;
- all finished conductor lengths;
- 3V3 rail and current measurements for zero through six sensors;
- one photo hash per sensor carrier, one mounted-photo hash per carrier, and a
  controller/fanout photo hash;
- carrier material, dimensions, strap slack/worn dimensions, and removal time;
- dimensioned-template tolerance overlay and the exact XIAO tape-lot and bond
  inspection record;
- canonical rig-evidence and axis-characterization hashes, candidate-manifest
  hash, source-session hash, all six-face raw means, and the bound signed axis
  maps;
- logic-analyzer file hash and channel legend;
- DSLogic raw capture, IKS3 session, locked manifest, handoff, sidecar, report,
  capture executable, byte-identical recorder/scorer, patch, firmware bundle,
  rig evidence, axis characterization, acquisition template, both logs, and
  corresponding-source provenance identities;
- D9-to-`TP_COMMIT` and D8-to-`TP_SNAPSHOT` continuity and analyzer-marker
  evidence, including final D8, the exact 52-transaction/1,422-SCL shutdown,
  simultaneous terminal marker, and five-second idle tail;
- immutable flash record with the named Bazel UF2 and provenance hashes,
  bootloader information and serial, application serial, board photo hash,
  logical controller identity, Zephyr lock revision, and boot ID;
  and
- explicit pass/fail/inconclusive result for Gates A-E and Gate 0A.

## Sources

A1-A16 were checked on 2026-08-12. A17-A18 were checked against the primary
vendor datasheets on 2026-08-29. A19-A23 were checked against Nordic, Seeed,
3M, and TI primary documentation on 2026-08-30. Molex pages returned an HTTP/2
error to direct `curl` from this host, so their current part facts were also
checked through Kagi search results and the linked Molex drawings. The
Adafruit, Pololu, Seeed, Nordic, TI, and GitHub pages returned HTTP 200 to
direct fetches.

| ID | Source | Fact used |
| --- | --- | --- |
| A1 | [Seeed Studio XIAO nRF52840 Plus](https://www.seeedstudio.com/Seeed-Studio-XIAO-nRF52840-Plus-p-6359.html) | SKU, standard form factor, dimensions, standard pins |
| A2 | [Adafruit TCA9548A product 2717](https://www.adafruit.com/product/2717) | Board identity, dimensions, mass, channels, downstream pull-up boundary |
| A3 | [Adafruit TCA9548A primary Eagle schematic](https://github.com/adafruit/Adafruit-TCA9548A-I2C-Multiplexer-PCB/blob/master/Adafruit%20TCA9548A.sch) | 10K upstream pull-ups, 10K RESET pull-up, 10 uF supply capacitor |
| A4 | [Adafruit LSM6DS3TR-C product 4503](https://www.adafruit.com/product/4503) | Board identity, dimensions, mass |
| A5 | [Adafruit LSM6DS3TR-C pinouts](https://learn.adafruit.com/adafruit-lsm6ds3tr-c-6-dof-accel-gyro-imu/pinouts) | VIN/GND/SCL/SDA/INT1 behavior, 10K I2C pull-ups, 0x6A address, 0.100-inch headers |
| A6 | [Adafruit LSM6DS3TR-C primary Eagle schematic](https://github.com/adafruit/Adafruit-LSM6DS3TR-C-PCB/blob/main/Adafruit_LSM6DS3.sch) | Non-contiguous JP1 signal pins, regulator, pull-ups, and onboard capacitors |
| A7 | [Molex SL housing 0050579405](https://www.molex.com/en-us/products/part-detail/50579405) | Five circuits, positive latch, mating family |
| A8 | [Molex SL terminal 0016020082](https://www.molex.com/en-us/products/part-detail/0016020082) | Female terminal, 24-30 AWG, selective gold, insulation limit drawing |
| A9 | [Molex SL header 0705430004](https://www.molex.com/en-us/products/part-detail/705430004) | Active five-circuit 2.54 mm vertical shrouded through-hole header and tail length |
| A10 | [Molex SL hand crimp tool 2002187000](https://www.molex.com/en-us/products/part-detail/2002187000) | Qualified 24-30 AWG SL hand crimp tool |
| A11 | [Adafruit Perma-Proto half-size board 1609](https://www.adafruit.com/product/1609) | Dimensions, mass, plated-hole and connected-row layout |
| A12 | [Pololu black 26 AWG stranded wire 2620](https://www.pololu.com/product/2620) | Gauge, construction, color, outer diameter |
| A13 | [Pololu red 26 AWG stranded wire 2622](https://www.pololu.com/product/2622) | Gauge, construction, color, outer diameter |
| A14 | [Pololu yellow 26 AWG stranded wire 2624](https://www.pololu.com/product/2624) | Gauge, construction, color, outer diameter |
| A15 | [Pololu blue 26 AWG stranded wire 2626](https://www.pololu.com/product/2626) | Gauge, construction, color, outer diameter |
| A16 | [Pololu white 26 AWG stranded wire 2629](https://www.pololu.com/product/2629) | Gauge, construction, color, outer diameter |
| A17 | [TI TCA9548A datasheet](https://www.ti.com/lit/ds/symlink/tca9548a.pdf) | Selectable bidirectional translating switch channels and deselected-channel disconnection |
| A18 | [ST LSM6DS3TR-C datasheet](https://www.st.com/resource/en/datasheet/lsm6ds3tr-c.pdf) | Direct output registers, BDU, DRDY pulse configuration and timing, FIFO reset mode |
| A19 | [Nordic nRF52840 Product Specification v1.5](https://files.seeedstudio.com/wiki/XIAO-BLE/nRF52840_PS_v1.5.pdf) | Normal-mode VDD range, GPIO high reference, and VDD-relative absolute I/O voltage limit |
| A20 | [Seeed XIAO nRF52840 Series hardware overview](https://wiki.seeedstudio.com/XIAO_BLE/) | `3V3_OUT` power-output label, 5V/VBUS power pin, D0-D9 mapping, and bootloader entry |
| A21 | [3M 9088-200 product data sheet](https://multimedia.3m.com/mws/media/1416427O/tape-ds-template.pdf) | Exact double-coated polyester-carrier tape identity |
| A22 | [Seeed XIAO nRF52840 Plus v1.1 KiCad project](https://files.seeedstudio.com/wiki/XIAO-BLE/Seeed_Studio_XIAO_nRF52840_Plus.zip) | Official schematic source identifies U6 as a TLV733P with 3.3 V value |
| A23 | [TI TLV733P product page and data sheet](https://www.ti.com/product/TLV733P) | Regulator-level 300 mA output rating and thermal/current-limit constraints, not board-level externally available current |
