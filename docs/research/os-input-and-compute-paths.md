# Operating-system input and compute paths

Accessed 2026-08-09. This report answers one bounded question: after sensing and
decoding a finger gesture, which current path can make a focused application
observe the intended keyboard behavior? A serial print, debug overlay, decoded
string, or firmware log is not operating-system integration.

## Decision summary

There is no single software injection API that is portable across desktop and
mobile operating systems. There are three defensible paths, conditional on the
target that the human has not chosen yet:

1. **Lowest-friction bench path:** stream timestamped sensor data to a Linux
   desktop, decode locally, and create a virtual keyboard with `uinput` through
   `libevdev`. Use a purpose-built target application to observe down, up,
   modifiers, repeat, text, composition, and recovery. Add Windows `SendInput`
   and macOS `CGEvent` adapters only after the event contract is stable.
2. **Cross-platform appliance path:** decode on the wearable MCU or a dedicated
   gateway and present a standards-conforming USB HID keyboard. Add Bluetooth
   LE HID over GATT (HOGP) only after USB passes the full event and fault matrix.
   This avoids host drivers and mobile keyboard-extension limits. It does not
   avoid pairing, host layout, rollover, focus, or disconnect testing.
3. **Research-flexible host-decoder path:** transport raw or lightly fused
   timestamped sensor frames to a desktop daemon, keep the decoder replaceable,
   and inject through `uinput`, `SendInput`, or `CGEvent`. This is the best path
   for data capture and model iteration, but not a portable product interface.

If Android itself is the first target, a user-selected input method editor
(IME) is a viable native product path. If iPhone or iPad is the first target and
passwords, phone fields, or apps that reject extensions are in scope, a custom
keyboard extension is categorically insufficient: use an external HID
peripheral. A browser-only implementation can type into its own cooperative web
application; it cannot become a system-wide keyboard.

The target platform remains a human product-boundary decision. This report does
not choose it and does not add a duplicate decision ticket.

## The event contract comes before the transport

The decoder must produce an ordered, causal event stream, not only completed
text:

```text
sensor frame -> intent/key decoder -> canonical key state -> output adapter
                                                        -> target application
```

Each event needs a monotonic timestamp, sequence number, source/session ID,
physical intent or semantic-text kind, key/usage, down/up state, modifier side,
and decoder confidence where applicable. The output adapter owns the set of
keys it believes are down. It must be able to emit a neutral state that releases
every key.

Keep two output contracts separate:

- **Physical-key contract:** HID usages, Linux key codes, Windows scan codes, or
  macOS virtual key events. The host's active layout and IME decide the resulting
  text. This is required for shortcuts, navigation, modifiers, holds, and the
  arbitrary-key claim.
- **Semantic-text contract:** a Unicode string committed by an Android IME,
  iOS keyboard extension, or Windows `KEYEVENTF_UNICODE`. This is useful for
  composition and corrected phrase text, but it is not evidence of physical key
  identity and cannot reproduce every shortcut or application key binding.

Do not silently fall back from one contract to the other. Record which one each
test uses.

## Standards appliance routes

### USB HID keyboard

USB HID is the lowest-risk product boundary. A conforming keyboard uses the
host's class driver, so the application does not need a vendor driver. The USB
HID specification defines reports as current state, not make/break messages.
The host infers presses and releases by comparing successive reports [S1].

The standard boot-keyboard input report is eight bytes: one modifier bitmap,
one reserved byte, and six non-modifier key slots. If more than six
non-modifier keys are held, the boot format reports `ErrorRollOver`. Modifiers
are a separate eight-bit bitmap. The host, not firmware, implements repeat delay
and repeat rate from the held state [S1]. Consequences:

- A simple boot descriptor provides broad firmware, login, and recovery
  compatibility, but only six-key non-modifier rollover.
- A report-protocol bitmap can advertise more rollover. That is a distinct
  descriptor and must be tested on every supported host. Do not call it NKRO
  until every advertised combination reaches the target application.
- Send a changed report immediately. Send an all-zero report for a deliberate
  release, before planned sleep, before firmware reset, and before switching
  output targets when possible.
- On an unplanned cable removal no final report can be sent. The test must prove
  how each host clears the removed device's state. On reconnect, start neutral
  and never replay a pre-disconnect held state.
- Caps Lock, Num Lock, Scroll Lock, Compose, and Kana LEDs are host state sent
  back in an output report. Firmware must not invent or preserve those states
  independently.

USB key usages describe positions/functions, not a universal character stream.
The same usage produces different characters under US QWERTY, French AZERTY,
German dead-key layouts, and active language IMEs. A product must either honor
the host layout or expose a clear, user-selected layout model. Firmware that
hardcodes US Shift combinations is not international text support.

Zephyr provides current USB and Bluetooth HID examples [S4][S5]. They reduce
firmware effort but do not prove the target device's descriptor, recovery, or
application behavior.

### Bluetooth LE HID over GATT

HOGP exposes HID reports through GATT so the host can route them into its HID
class path [S3]. It is viable for normal typing on current desktop and mobile
hosts, subject to a real interoperability matrix. The specification requires:

- a HID Service, Battery Service, and Device Information Service;
- a Report Map and report characteristics, with boot keyboard characteristics
  when boot mode is supported;
- bonding by the host and an encrypted link for HID Service reads, writes, and
  notifications;
- suspend/control-point behavior, bond lifecycle, and reconnect behavior; and
- battery reporting without excessive polling.

HOGP 1.2 defines application-observed end-to-end latency, not merely radio
latency. Its ordinary default mode uses a 7.5-15 ms transport interval. Its new
Shorter Connection Intervals feature can negotiate 1.25-5 ms fast intervals
and sub-7.5 ms operation, but it depends on Bluetooth Core 6.2 support in both
host and device. Treat that as a future optimization, not a prototype
assumption. Connection interval is only one latency component; sensor window,
decoder, scheduling, retransmission, OS dispatch, and application response still
need timestamps [S3].

Pairing is user-visible product behavior. Test first pair, wrong passkey,
cancelled pair, bond deletion on either side, host switching, radio loss,
reconnection, sleep, low battery, and firmware update. An authenticated
passkey-capable design is preferable where the enclosure can provide a safe
pairing affordance. At minimum the link remains encrypted as HOGP requires.

Do not use the HID service as an undocumented bulk sensor pipe. A bench device
may expose a separate vendor GATT service or USB CDC interface for versioned raw
frames while exposing HID for output. Disable or authenticate the debug service
in a product.

### Payload scale and honest power bounds

An eight-byte boot report for each state change is small. Ten state changes per
second are only 80 bytes/s of HID payload before protocol overhead. Raw sensing
is the bandwidth driver. For example, two wrists times six axes times 16 bits
times 200 Hz is 4,800 bytes/s before timestamps, sequence numbers, framing, and
retransmission; at 1 kHz it is 24,000 bytes/s. These are arithmetic examples,
not measured topology throughput.

An nRF52840-class part is a credible combined USB/BLE and small-model test
target: Nordic specifies a 64 MHz Cortex-M4F, 1 MB flash, and 256 KB RAM, and
Zephyr demonstrates a 20 KB accelerometer gesture model with TensorFlow Lite
Micro [S30][S31]. This proves toolchain availability, not that the invisible
keyboard decoder fits or meets latency. Nordic lists 4.8 mA peak transmit
current for one radio condition [S30]; it is not a battery-runtime estimate.
Gate 9 still requires measured whole-device average/peak power, battery mass,
runtime, and recharge time.

## Desktop host routes

### Linux: `uinput`

The Linux kernel documents `uinput` as the user-space interface for creating a
virtual input device and delivering events to user-space and in-kernel
consumers. It shows `EV_KEY` down and up events separated by `EV_SYN`/
`SYN_REPORT`, and recommends `libevdev` for new software because it is less
error-prone than raw ioctls [S6].

This is the canonical bench route:

- Create one virtual keyboard with exactly the supported `KEY_*` capabilities.
- Write access to `/dev/uinput` is privileged. Install a narrow udev/group rule
  or a small privileged device-creation boundary; do not run the decoder and
  model as root.
- Wayland is not a blocker at this layer because the compositor receives a
  kernel input device. Container, seat, and distribution policy still need
  tests.
- Hold behavior is event value 1 for down and 0 for up; Linux also defines value
  2 for autorepeat, but the product contract should let the active host own
  normal repeat unless a target-specific requirement proves otherwise.
- Close/recreate and process-crash tests must leave the host neutral. A
  supervisor should reject new presses until the virtual device exists and a
  neutral synchronization record has been acknowledged.

### Windows: `SendInput` versus VHF

`SendInput` inserts an ordered array of keyboard events into the system input
stream. `KEYEVENTF_KEYUP` represents release. `KEYEVENTF_SCANCODE` expresses a
physical key independent of the active layout; `KEYEVENTF_UNICODE` produces a
`VK_PACKET` text path to the foreground application [S7][S8].

It is suitable for a prototype and a user-session accessibility tool, with hard
limits:

- User Interface Privilege Isolation permits injection only into applications
  at equal or lower integrity. A zero return does not identify UIPI as the
  cause. An elevated editor is therefore a required negative test [S7].
- It is not a route into the UAC secure desktop, logon desktop, or other trusted
  system surfaces. The secure desktop exists to prevent input/output spoofing
  [S12].
- Existing keyboard state is not reset for the caller. Track every injected
  down state and issue paired ups on cancellation, focus loss, transport loss,
  and orderly shutdown.
- Unicode injection commits text but does not prove the scan-code, modifier,
  shortcut, dead-key, or physical-layout contract. Use scan codes for claim B.
- Windows implements dead keys, layout translation, and typematic repeat after
  the physical/scan-code layer. Test the active layout instead of predicting
  final characters in the adapter [S9].
- FilterKeys can change acceptance delay, bounce handling, and repeat. StickyKeys
  can serialize modifiers. The product must remain usable and must report the
  active accessibility condition rather than changing it [S13].

Windows Virtual HID Framework (VHF) is the deeper product-like software route.
It enumerates a virtual HID tree, but Microsoft's current VHF source driver is
kernel-mode KMDF or WDM, not a normal user-mode helper [S10]. Production means
WDK work, INF installation, cleanup/power-state handling, an attack surface,
and driver signing. Current Windows requires new kernel drivers to be signed
through the Hardware Dev Center; establishing the dashboard account requires
an EV certificate, and production certification adds HLK policy work [S11].
That cost is hard to justify when a physical HID appliance needs no custom
keyboard driver. The UWP `InputInjector` alternative requires a restricted
capability and is not a general distribution escape hatch.

### macOS: `CGEvent` versus virtual HID

Core Graphics can create key-down/key-up events and `CGEvent.post(tap:)` posts
them into a Quartz event stream [S14]. A host daemon must check/request
post-event Accessibility trust with Apple's post-event or Accessibility APIs,
and the user must allow the app to control the Mac [S15]. This is the macOS
prototype route.

Do not request Input Monitoring merely to inject. Input Monitoring is the
separate permission for observing global keyboard/mouse/trackpad input. The
target test application can observe its own events without turning the decoder
into a global key logger.

The boundary is similar to Windows user-mode injection: prove ordinary apps,
layouts, Command/Option/Control/Shift, holds, repeat, Mission Control/system
shortcuts, focus changes, lock/sleep, and secure fields. Do not claim FileVault,
login-window, password, or protected-system support from a successful
`CGEvent` call. A physical USB/BLE keyboard is the candidate for those paths.

Apple now documents `HIDVirtualDevice` in Core HID as a virtual peripheral for
testing/development [S16]. Apple also documents a managed DriverKit virtual-HID
entitlement whose request uses Apple's system-extension entitlement process,
and system extensions must be App Store distributed or notarized [S17][S18].
The official pages inspected did not establish a generally grantable,
App-Store-safe Core HID virtual-keyboard distribution contract. Older
`IOHIDUserDeviceCreateWithProperties` exists as an API, but API existence is not
distribution permission. Treat all macOS virtual-HID routes as gated until
Apple grants the exact entitlement for the shipping team and target. Do not
build the product plan around an unapproved entitlement.

## Mobile routes

### Android

There are two strong paths and two weak or mis-scoped paths.

**External HID peripheral:** Android's input stack maps HID usages to Linux key
codes, then through the device key-layout map to Android key codes, maintains
modifier state, and dispatches the event to the focused window and then the IME
[S24]. USB HID or BLE HOGP therefore gives system-level hardware keyboard
behavior. Layout selection, OEM variations, system shortcuts, sleep, and every
advertised key still require device-matrix testing [S24].

**Android IME:** an `InputMethodService` declared with `BIND_INPUT_METHOD` can
use the focused field's `InputConnection` to send individual key events or edit
text around the cursor. It receives the field's input type and can handle
composition [S21]. The user must install, enable, and select it. Use
`commitText`/composition for semantic language output and key events for the
bounded physical-key subset. The official guide explicitly calls out password
input handling; never show, retain, or transmit password content. Unlike the
iOS custom-keyboard contract, the inspected Android guide does not specify an
automatic platform replacement in password fields, so this needs real device,
enterprise-policy, and app tests rather than an assumed block.

An active IME bound by a foreground app is treated as foreground for Android's
background-service limits [S25]. A separate companion sensor service is not
automatically exempt. Continuous BLE decoding outside the active IME generally
needs the appropriate foreground-service design and visible notification, plus
runtime Nearby Devices permission. On Android 12+, communicating with paired
devices requires `BLUETOOTH_CONNECT`; scanning and advertising have separate
runtime permissions [S23][S25].

**Android as a keyboard for another host:** API 28's
`BluetoothHidDevice` exposes the classic Bluetooth HID Device profile. Only one
app may register, and Android automatically unregisters it when the app is no
longer foreground [S22]. This can make a phone decoder a keyboard for a laptop;
it does not inject into the same phone and it is not BLE HOGP.

**AccessibilityService:** this is not the default keyboard route. The user must
enable an accessibility service; accessibility focus differs from input focus;
node actions and dispatched touch gestures are not a complete down/up,
modifier, repeat, rollover, or arbitrary-app keyboard contract [S26]. Use it
only if the chosen product is explicitly an assistive service whose policy and
user benefit justify that privilege.

### iOS and iPadOS

**External keyboard:** a supported USB or Bluetooth HID peripheral is the only
verified path with the shape of a general hardware keyboard. It can type while
another app has focus; Full Keyboard Access additionally lets a user navigate
the system with an external keyboard [S20]. Test normal typing separately from
Full Keyboard Access because the latter can consume shortcuts.

**Custom keyboard extension:** a user can enable and select an extension and it
can insert text at the current text input object. Its limits are explicit and
hard [S19]:

- iOS replaces it with the system keyboard in every `secureTextEntry` field;
- it is ineligible for `phonePad` and `namePhonePad` fields;
- an app may reject all custom keyboard extensions;
- it cannot select text or access host-app cut/copy/paste controls;
- network access and a shared container are off by default; enabling
  `RequestsOpenAccess` expands the sandbox and exposes typed data to greater
privacy risk; and
- it runs as the selected input extension, not as a general always-on system
  event injector.

Therefore a custom keyboard extension can support a phrase-entry product for
ordinary fields, but cannot satisfy arbitrary system-wide input, password
entry, or full external-keyboard equivalence. There is no verified public iOS
API for an ordinary app to inject arbitrary key events into other apps. Do not
plan around Core HID virtual-device API names without a granted entitlement and
an explicit App Store distribution ruling.

### Extended reality boundary

No general cross-app software injection contract was verified for visionOS or
other headset platforms in this pass. A standards-compliant physical keyboard
is the only portable candidate; target-specific text, focus, privacy, and input
APIs require a new verification pass after the human chooses an XR platform.
A browser or phone companion does not bridge that missing contract.

## Browser-only feasibility

A cooperative web application can read its own custom device and update its
own editor. WebHID can open an uncommon HID interface in a secure context after
a user chooser; permissions and disconnect events are origin-scoped [S28]. It
is experimental/not uniformly supported. Chrome deliberately blocks generic
keyboard top-level collections and other protected HID usages from WebHID
because they can generate trusted input [S29]. A separate vendor-defined HID
interface can expose sensor frames to an authorized site, but that site still
does not become an OS keyboard.

JavaScript-created `KeyboardEvent` objects dispatched with `dispatchEvent()`
have `isTrusted == false` [S27]. They notify listeners; they do not grant the
browser's trusted default editing behavior in arbitrary apps, cross origins, or
outside the browser. WebDriver can generate automation input through an
external browser-control protocol, but it is a test interface, not a keyboard
product. Clipboard APIs are permission/user-activation constrained and do not
preserve key-down, shortcut, secure-field, or continuous typing semantics.

Verdict: browser-only is viable for a controlled demo, decoder training UI, or
application-specific editor. It is impossible as the general invisible
keyboard described by claim B.

## Where decoding can run

| Placement | Best use | Advantages | Costs and hard limits |
| --- | --- | --- | --- |
| Wearable MCU | Small causal model, direct USB/BLE HID | Lowest transport latency, offline, raw sensor data need not leave device, simplest appliance | Tight RAM/flash/CPU, harder debugging and updates, model must be profiled on the exact MCU, whole-device power unknown |
| Dedicated gateway | Larger local model plus portable HID output | Keeps one cross-platform host contract, easier model replacement, can fuse several synchronized sensors | Extra BOM, battery, enclosure, link, clock, boot, and update failure modes |
| Phone | Mobile model/NPU, calibration UI | No extra compute box, good UI and local privacy when processing stays on device | Same-phone output is Android IME or restricted iOS extension; Android phone-to-PC HID must stay foreground; iOS arbitrary system injection is unavailable |
| Desktop | Research and rapid decoder iteration | Most compute, easiest raw logging/profiling, model updates, deterministic app harness | Platform adapters/permissions, install burden, no pre-login guarantee, host privacy and support cost |
| Remote service | None in the first prototype | Potentially large model | Network latency/outage, keystroke and biometric privacy, credential-field risk, cost, and no reason before local feasibility |

The canonical architecture should keep a versioned transport boundary so the
same sensor data can first feed a desktop model and later a gateway/MCU model.
That is not a permanent dual product path. It is a research seam with deletion
criteria: once the chosen model passes on the appliance, production output is
one direct HID path and raw streaming is removed or locked behind explicit
diagnostic mode.

Model updateability is inversely related to how close compute sits to the
sensor. Desktop/phone models can use signed application updates and rollback.
MCU/gateway firmware needs signed images, power-loss-safe update, rollback, a
versioned model/firmware pair, and a neutral keyboard state before reboot. A
failed update must fail silent as a keyboard, never leave a modifier down.

## State, layout, accessibility, privacy, and recovery rules

1. **One owner of key state.** The final adapter owns a set of down keys. It
   rejects duplicate down/up transitions, records them as decoder defects, and
   can release the entire set.
2. **Neutral startup.** A new process, USB enumeration, BLE connection, IME
   binding, or focus session begins with no keys down. Never restore a held key
   from persistent state.
3. **Fault deadline.** Planned shutdown sends all-up first. Unplanned transport
   loss starts a host/device watchdog; Gate 0 requires application-observed
   neutral state within 250 ms and zero stuck repeat/modifier state.
4. **Host-owned repeat.** Hold one down state and let the host's active repeat
   delay/rate and accessibility settings act. Only an IME semantic-text mode may
   need an explicit repeat policy, which must be labeled and tested separately.
5. **No focus guessing.** Output goes only to the OS-selected focused target.
   A host daemon may stop accepting new gestures during lock/switch and release
   held keys before a known focus transition. It must not select windows or
   paste into a remembered target.
6. **Layout is an input.** Record OS layout, language, IME, Caps/Num state, and
   accessibility settings. Test a US layout, at least one non-US dead-key
   layout, and at least one composing IME before claiming international input.
7. **Modifiers are state, not text.** Test left/right Shift, Control, Alt/Option,
   GUI/Command, AltGr where applicable, chords, holds, and releases. Test
   StickyKeys and FilterKeys without disabling the user's settings.
8. **Privacy defaults off.** Production telemetry must not contain raw motion,
   inferred keystrokes, surrounding text, clipboard contents, or focused-app
   names by default. Research capture requires an explicit armed session,
   local encryption, retention/deletion control, and no network egress.
9. **Passwords never become training data.** Secure-field tests use generated,
   non-secret strings and record only expected/observed length, event counts,
   and a local comparison result. No plaintext log, screenshot, or telemetry.
10. **Recovery is observable.** Surface disconnected, decoder-unready, pairing,
    permission, update, and degraded-power states. Silent fallback from raw key
    events to pasted/corrected text is forbidden.

## Mapping to the ten feasibility gates

| Gate | What this input path must prove |
| --- | --- |
| 0 - stream integrity | Full sensor-to-app topology for 60 minutes and 10,000 deterministic down/up records; sequence accounting; stage timestamps; 10 forced faults; p95 <= 100 ms, p99 <= 200 ms; all-up <= 250 ms |
| 1 - isolated intent | The target application, not the adapter log, observes each isolated intended activation and quiet-period false activations |
| 2 - finger/key identity | Physical-key mode preserves exact top-1 key class with language correction disabled; semantic-text mode cannot stand in for claim B |
| 3 - live uncorrected text | A focused ordinary application receives causal live text from current events; raw and final application text remain traceable |
| 4 - corrected phrases | Correction runs before semantic commit or before physical events are chosen, and raw/pre-language/final streams remain separate |
| 5 - arbitrary text/code | Every advertised key, key-up, hold, repeat, modifier, symbol, shortcut, layout, and rollover result is observed in an isolated target app; no stuck destructive state |
| 6 - session/remount transfer | Fresh mount and fresh OS input session start neutral; no prior keyboard state, layout cache, or calibration leaks into scoring |
| 7 - person transfer | Output adapter stays fixed while unseen people are tested; per-person decoder results cannot be hidden by flawless HID transport |
| 8 - ordinary negatives | The complete armed output path runs during ordinary activity; false modifier/shortcut events count even if the target app ignores them |
| 9 - extended use | Ninety-minute app-observed behavior includes disconnect/recovery, permission/pairing burden, whole-system power, comfort, and no late stuck state |

## Application-observed verification matrix

The harness needs a target application under project control. It records its
own native key callbacks plus rendered editor state, and acknowledges every
received event over a separate test channel. A browser `contenteditable` page
is an additional target, not the sole oracle. Use a second real target such as
a terminal/editor, but score its final state rather than assuming delivery.

| Test | Stimulus | Application observation | Required result |
| --- | --- | --- | --- |
| Basic state | Each advertised key down, 30 ms hold, up | Native down/up, logical key, physical code/scan code, timestamps, final text | One ordered pair; correct code and text for active layout |
| Modifiers | Left/right modifier plus key, overlapping releases | Modifier flags and shortcut's harmless visible effect | Correct side where exposed, correct chord, every key released |
| Hold/repeat | Hold below and above configured delay | Down/repeat/up callbacks and text count | Onset within 50 ms and rate within 5 percent of active host setting |
| Rollover | Every advertised chord size and selected adversarial combinations | Simultaneous state and resulting callbacks | No hidden keys; boot path reports/handles >6 limit honestly |
| Layout/dead key | US, non-US dead key, AltGr, composing IME | `key`/`code`, composition events, final Unicode text | Physical and semantic expectations stay distinct and exact |
| Corrected text | Known phrase plus deliberate correction | Pre-language events, commits, snapshots, final editor | Causal trace; no future/test-phrase leakage |
| Focus switch | Switch between two test windows before, during, after a hold | Both apps' event logs and state | No event to a remembered target; no stuck key in either app |
| Accessibility | StickyKeys/FilterKeys or platform equivalents enabled | Callbacks, accepted/rejected keys, repeat | Honors setting; no programmatic disabling or false failure claim |
| Permission loss | Revoke uinput access, macOS Accessibility, Android selection/permission | Target log plus explicit product status | No silent typing, no fallback paste, clear recovery instructions |
| Integrity boundary | Windows normal and elevated target; UAC secure desktop negative | Target event count | `SendInput` normal case works; elevated/secure limits are reported, not bypassed |
| Mobile secure field | Generated password-like input | Only local pass/fail and length | External HID tested; iOS custom extension is replaced and marked unsupported |
| Phone field/app opt-out | iOS phone-pad and extension-rejecting test app | Which keyboard appears and resulting text | Extension route marked unsupported; external HID tested separately |
| Disconnect | Remove cable/radio during each modifier and repeat | Target's held-state/repeat after 250 ms | Neutral by deadline, zero further repeat, clean neutral reconnect |
| Sleep/wake/lock | Sleep host and device in every key state | Pre/post-wake target and OS state | No replay, no stuck key, explicit re-pair/reconnect status |
| Crash/update | Kill decoder and reset/update output device mid-chord | Target state and supervisor status | All-up or device-removal neutral; restart refuses stale state |
| Long run | Gate 0 and Gate 9 durations | Sequence/latency panel and final app snapshots | Meets gate bounds with application denominator, not firmware log denominator |

Clock the stages separately: physical reference, first sensor sample, complete
feature window, decoder decision, adapter call/report, OS acknowledgement where
available, application callback, and visible/rendered state. If two devices do
not share a clock, establish and report the synchronization method and residual.
Never subtract unrelated wall clocks and call the result latency.

## Prototype and product recommendations

### Prototype 0: deterministic output contract

Before connecting a gesture model, replay a deterministic event file through
Linux `uinput` into the target harness. Pass Gate 0, including fault injection,
layout, repeat, and state cleanup. Then connect the live sensor transport. This
isolates decoder errors from injection errors.

### Prototype 1: research decoder

Use a timestamped USB serial or authenticated BLE vendor service into a local
desktop decoder, then the same `uinput` adapter. Preserve immutable raw frames,
decoder events, injected events, and application acknowledgements. Windows and
macOS adapters are compatibility experiments after Linux is green, not three
simultaneous implementations.

### Appliance proof

Port the frozen causal decoder to the wearable MCU if it fits measured RAM,
flash, inference latency, and power. Otherwise use a dedicated local gateway.
Expose USB HID first. Run the same target-app matrix on Linux, Windows, macOS,
Android, and iPadOS. Add BLE HOGP only after USB is clean, then repeat pairing,
radio fault, sleep, bond, power, and host-layout tests.

### Target-forced branches

- Android-native product: IME plus local sensor service, with external HID as
  the arbitrary-key/password benchmark.
- iOS/iPadOS full-scope product: external HID appliance. An extension-only
  product must explicitly exclude secure fields, phone pads, rejecting apps,
  pre-login/system surfaces, and hardware-equivalent shortcuts.
- Windows software-only product: accept UIPI/secure-desktop exclusions or fund
  a signed VHF driver. The latter remains inferior to hardware HID for a
  cross-platform device unless a driver-specific feature is essential.
- macOS software-only product: accept Accessibility consent and protected-
  surface exclusions. Do not budget a virtual-HID product until Apple grants
  and confirms distribution for the required entitlement.
- Browser product: scope it to one cooperative site/application. It is not the
  general invisible keyboard.

## Cost and next-ticket implications

Ticket `Cost the prototype and product paths` should price three distinct
packages, not one blended BOM:

1. Host research: sensor boards/radios plus no gateway, but desktop packaging,
   udev/Accessibility permission support, Windows/macOS adapter work, and test
   devices.
2. USB/BLE appliance: one dual-role MCU if the model fits, otherwise sensor MCU
   plus gateway; add battery, charging, enclosure, certification, fixture,
   signed firmware/update, and a five-platform interoperability lab.
3. Mobile-native: Android IME/service development versus an iOS extension with
   explicit product exclusions. Do not count the extension as equivalent to
   external HID.

The largest avoidable software cost is a Windows or macOS virtual-HID driver
and its signing/entitlement lifecycle. The largest avoidable hardware cost is a
gateway before profiling proves it is needed. The decisive prototype ticket
should therefore specify deterministic `uinput` Gate 0, live host decoding,
then USB HID appliance proof in that order. It should not begin with BLE
pairing polish, a custom driver, or a mobile extension.

## Sources

All sources were accessed 2026-08-09. Standards and official platform/vendor
documentation establish API and protocol boundaries. Sample firmware proves a
supported implementation route, not product behavior.

| ID | Source | What it establishes |
| --- | --- | --- |
| S1 | [USB-IF, Device Class Definition for HID 1.11](https://www.usb.org/sites/default/files/hid1_11.pdf) | Report state, boot keyboard format, six-key limit, modifiers, host repeat, LEDs, protocol behavior |
| S2 | [USB-IF, HID Usage Tables 1.7](https://www.usb.org/document-library/hid-usage-tables-17) | Current keyboard/keypad usage identifiers and usage semantics |
| S3 | [Bluetooth SIG, HID over GATT Profile 1.2](https://www.bluetooth.com/specifications/specs/hid-over-gatt-profile-hogp/) | Services, reports, bonding/encryption, suspend, reconnect, report intervals, SCI latency modes |
| S4 | [Zephyr, USB HID keyboard sample](https://docs.zephyrproject.org/latest/samples/subsys/usb/hid-keyboard/README.html) | Maintained MCU USB keyboard implementation route |
| S5 | [Zephyr, Bluetooth HID peripheral sample](https://docs.zephyrproject.org/latest/samples/bluetooth/peripheral_hids/README.html) | GATT HID service and authenticated/encrypted pairing example |
| S6 | [Linux kernel, uinput](https://docs.kernel.org/input/uinput.html) | Virtual input creation, EV_KEY/SYN_REPORT, libevdev recommendation |
| S7 | [Microsoft, SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput) | Ordered user-mode injection and UIPI limit/failure behavior |
| S8 | [Microsoft, KEYBDINPUT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-keybdinput) | Down/up, scan-code, extended-key, and Unicode/VK_PACKET modes |
| S9 | [Microsoft, Keyboard Input Overview](https://learn.microsoft.com/en-us/windows/win32/inputdev/about-keyboard-input) | Layout, scan/virtual/character mapping, repeat, dead keys, focus |
| S10 | [Microsoft, Virtual HID Framework](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/virtual-hid-framework--vhf-) | Kernel-mode KMDF/WDM HID source driver architecture |
| S11 | [Microsoft, Driver Signing Policy](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-policy--windows-vista-and-later-) | Hardware Dev Center, EV certificate, signing/certification burden |
| S12 | [Microsoft, UAC secure desktop policy](https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/security-policy-settings/user-account-control-switch-to-the-secure-desktop-when-prompting-for-elevation) | Protected desktop input/output spoofing boundary |
| S13 | [Microsoft, FILTERKEYS](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-filterkeys) | Acceptance delay, bounce, and repeat accessibility behavior |
| S14 | [Apple, CGEvent post](https://developer.apple.com/documentation/coregraphics/cgevent/post(tap:)) | Quartz event-stream posting API |
| S15 | [Apple, Allow accessibility apps to access your Mac](https://support.apple.com/guide/mac-help/allow-accessibility-apps-to-access-your-mac-mh43185/mac) | User consent for apps that control macOS |
| S16 | [Apple, Creating virtual devices](https://developer.apple.com/documentation/corehid/creatingvirtualdevices) | Core HID virtual-device testing/development route |
| S17 | [Apple, DriverKit Family virtual HID entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.driverkit.family.hid.virtual.device) | Managed entitlement requirement and request path |
| S18 | [Apple, System Extensions](https://developer.apple.com/documentation/systemextensions) | App Store or notarized distribution requirement |
| S19 | [Apple, Custom Keyboard](https://developer.apple.com/library/archive/documentation/General/Conceptual/ExtensibilityPG/CustomKeyboard.html) | Secure/phone-field/app opt-out and open-access limits |
| S20 | [Apple, Control iPad with an external keyboard](https://support.apple.com/guide/ipad/control-ipad-with-an-external-keyboard-ipad5f765d6f/ipados) | External keyboard and Full Keyboard Access behavior |
| S21 | [Android, Create an input method](https://developer.android.com/develop/ui/views/touch-and-input/creating-input-method) | IME declaration, InputConnection text/key delivery, field types, composition |
| S22 | [Android, BluetoothHidDevice](https://developer.android.com/reference/kotlin/android/bluetooth/BluetoothHidDevice) | Phone-as-HID registration, single app, foreground lifetime |
| S23 | [Android, Bluetooth permissions](https://developer.android.com/develop/connectivity/bluetooth/bt-permissions) | Android 12+ scan/advertise/connect runtime permissions |
| S24 | [AOSP, Keyboard devices](https://source.android.com/docs/core/interaction/input/keyboard-devices) | HID-to-Linux-to-Android mapping and focused-window dispatch path |
| S25 | [Android, Background Execution Limits](https://developer.android.com/about/versions/oreo/background) | IME foreground binding and service lifetime constraints |
| S26 | [Android, Create an accessibility service](https://developer.android.com/guide/topics/ui/accessibility/service) | User service, node/global actions, focus distinction, gesture scope |
| S27 | [MDN, Event.isTrusted](https://developer.mozilla.org/en-US/docs/Web/API/Event/isTrusted) | Script-dispatched events are untrusted |
| S28 | [WICG, WebHID](https://wicg.github.io/webhid/) | Chooser/permission model, protected usages, device security model |
| S29 | [Chrome, Connect to uncommon HID devices](https://developer.chrome.com/docs/capabilities/hid) | Chrome permissions and generic keyboard/security-sensitive HID blocking |
| S30 | [Nordic, nRF52840](https://www.nordicsemi.com/Products/nRF52840) | MCU CPU, memory, USB/BLE class, not system runtime |
| S31 | [Zephyr, TFLM Magic Wand](https://docs.zephyrproject.org/latest/samples/modules/tflite-micro/magic_wand/README.html) | Maintained 20 KB accelerometer gesture-model example |

## Confidence and unresolved facts

Confidence is high in the USB HID, HOGP, Linux `uinput`, Windows UIPI/VHF,
Android IME/external-HID, iOS custom-keyboard, and browser security boundaries.
Confidence is medium in host-specific BLE interoperability until physical tests
cover named OS/device versions. Confidence is low in any claim about a
shippable Apple virtual-HID entitlement because the public API pages do not
establish approval or App Store eligibility for this product.

Unknown until the human chooses a target and a prototype measures it: supported
layouts and IMEs, acceptable secure/pre-login scope, real USB/BLE end-to-end
latency, host BLE interval negotiation, model size/inference time, raw sensor
throughput, whole-device power/runtime, permission/setup tolerance, and the
exact key/rollover set. These are not reasons to block a bench prototype; they
are its measurement contract.
