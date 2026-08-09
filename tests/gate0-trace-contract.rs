use std::{
    env,
    fs::{self, OpenOptions},
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use invisible_keyboard::{
    gate0::{GATE0A_REPORT_SCHEMA, Gate0Status, score_gate0a},
    gate0_trace::{
        GATE0_CAPTURE_SIDECAR_SCHEMA, GATE0_REQUESTED_SAMPLES, GATE0_SAMPLE_RATE_HZ,
        Gate0CapturePaths, Gate0TraceError, Gate0TraceEvidence, Gate0TraceViolationCode,
        MAX_GATE0_TRACE_VIOLATION_WITNESSES, reduce_gate0_capture,
    },
    layout::Hand,
    manifest::{
        ACQUISITION_MANIFEST_SCHEMA, AcquisitionManifest, CANONICAL_ACQUISITION_PROFILE,
        CORE28_LAYOUT_ID, HandManifest, ManifestHand, SensorManifest, format_sha256, sha256,
    },
    protocol::{
        ALL_SENSORS_VALID, PROTOCOL_VERSION, QuiesceReceipt, QuiesceRequest, SENSOR_COUNT,
        SensorFrame, SensorReading,
    },
    session::{ControllerStream, FinalizedSession, SessionHeader, SessionWriter},
};
use serde_json::json;

const INT1_MASK: u16 = 0x003f;
const D8_MASK: u16 = 1 << 6;
const D9_MASK: u16 = 1 << 7;
const SDA_MASK: u16 = 1 << 8;
const SCL_MASK: u16 = 1 << 9;
const RESERVED_MASK: u16 = 1 << 10;
const NOMINAL_EDGE_INTERVAL_TICKS: u64 = 9_615;
const NORMAL_MARKER_TICKS: u64 = 4;
const BOOT_MARKER_TICKS: u64 = 40;
const SENSOR_STARTUP_DELAY_TICKS: u64 = 40_000;
const TERMINAL_MARKER_TICKS: u64 = 300;
const TERMINAL_PREMARKER_IDLE_TICKS: u64 = 400;
const TERMINAL_IDLE_MINIMUM_SAMPLES: u64 = 10_000_000;
const SHUTDOWN_TRANSACTION_COUNT: usize = 52;
const STOP_WINDOW_FIRST_BYTE: u64 = 14_416_000_000;
const STOP_WINDOW_LAST_BYTE: u64 = 14_417_000_000;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
enum ShutdownMutation {
    Omit(usize),
    Duplicate(usize),
    Reorder(usize),
    Nack(usize),
    Corrupt(usize),
    Truncate(usize),
}

#[derive(Clone, Copy, Debug)]
enum StartupMutation {
    Omit(usize),
    Corrupt(usize),
    Nack(usize),
}

#[derive(Clone, Debug, Default)]
struct Scenario {
    missing_boot_anchor: bool,
    short_boot_marker: bool,
    startup_mutation: Option<StartupMutation>,
    startup_sensor_count: Option<usize>,
    premature_int1_enable: bool,
    startup_arm_sensor_count: Option<usize>,
    startup_transcript: Option<String>,
    arm_int1_before_stop: bool,
    arm_extra_edge_before_first_mux: bool,
    startup_reset_polls: Option<usize>,
    startup_reset_never_clears: bool,
    startup_short_mux_settle: bool,
    startup_short_reset_delay: bool,
    startup_int1: bool,
    known_quiet_pulse_sensor: Option<usize>,
    pre_snapshot_decimation: bool,
    pre_snapshot_extra_edge: bool,
    priming_idle_ticks: Option<u64>,
    premature_snapshot: bool,
    premature_commit: bool,
    wrong_mux: bool,
    ready_before_final: bool,
    missing_d9: bool,
    d8_during_d9: bool,
    next_edge_crossing: bool,
    four_status_polls: bool,
    slow_odr: bool,
    wrong_raw_payload: bool,
    decimation: bool,
    ambiguous_bus_transition: bool,
    short_normal_marker: bool,
    many_reserved_transitions: bool,
    missing_int1: bool,
    malformed_i2c: bool,
    misordered_d9: bool,
    jitter_average_hides_outliers: bool,
    bad_capture_log: bool,
    oversized_capture_log_line: bool,
    wrong_firmware_log_hash: bool,
    unterminated_capture_summary: bool,
    mismatched_driver_receipt: bool,
    trailing_driver_receipt_token: bool,
    summary_not_final: bool,
    bus_activity_before_boot: bool,
    marker_bus_transition_overlap: bool,
    extra_acquisition_after_final: bool,
    pre_cut_unmatched_edge: bool,
    int1_at_final_cut: bool,
    post_cut_int1: bool,
    shutdown_mutation: Option<ShutdownMutation>,
    premarker_interrupt: bool,
    premarker_idle_ticks: Option<u64>,
    terminal_marker_width_ticks: Option<u64>,
    staggered_terminal_marker: bool,
    duplicate_terminal_marker: bool,
    post_marker_mask: Option<u16>,
    terminal_idle_samples: Option<u64>,
}

struct Fixture {
    manifest: AcquisitionManifest,
    session: FinalizedSession,
    raw_capture: PathBuf,
    sidecar: PathBuf,
    capture_binary: PathBuf,
    firmware_bundle: PathBuf,
}

impl Fixture {
    fn reduce(&self) -> Result<Gate0TraceEvidence, Gate0TraceError> {
        reduce_gate0_capture(
            &self.manifest,
            &self.session,
            Gate0CapturePaths {
                raw_capture: &self.raw_capture,
                sidecar: &self.sidecar,
            },
        )
    }
}

#[derive(Clone, Debug)]
struct Waveform {
    words: Vec<u16>,
    state: u16,
    first_acquisition_edges: [Option<u64>; SENSOR_COUNT],
    first_snapshot_tick: Option<u64>,
}

impl Waveform {
    fn new() -> Self {
        let state = SDA_MASK | SCL_MASK;
        Self {
            words: vec![state],
            state,
            first_acquisition_edges: [None; SENSOR_COUNT],
            first_snapshot_tick: None,
        }
    }

    fn tick(&self) -> u64 {
        self.words.len() as u64 - 1
    }

    fn next_tick(&self) -> u64 {
        self.words.len() as u64
    }

    fn set(&mut self, mask: u16, high: bool) {
        let next = if high {
            self.state | mask
        } else {
            self.state & !mask
        };
        self.set_state(next);
    }

    fn set_state(&mut self, state: u16) {
        if state != self.state {
            self.state = state;
            self.words.push(state);
        }
    }

    fn hold(&mut self, ticks: u64) {
        self.words
            .extend(std::iter::repeat_n(self.state, ticks as usize));
    }

    fn hold_until(&mut self, tick: u64) {
        if self.tick() < tick {
            self.hold(tick - self.tick());
        }
    }

    fn pulse(&mut self, mask: u16, width_ticks: u64) {
        self.set(mask, true);
        self.hold(width_ticks.saturating_sub(1));
        self.set(mask, false);
    }

    fn boot_anchor(&mut self, scenario: &Scenario) {
        self.hold(8);
        if scenario.bus_activity_before_boot {
            self.i2c_start();
            self.i2c_stop();
        }
        if scenario.missing_boot_anchor {
            return;
        }
        let d8_width = if scenario.short_boot_marker {
            NORMAL_MARKER_TICKS
        } else {
            BOOT_MARKER_TICKS
        };
        self.pulse(D8_MASK, d8_width);
        self.hold(BOOT_MARKER_TICKS - 1);
        self.pulse(D9_MASK, BOOT_MARKER_TICKS);
        self.hold(BOOT_MARKER_TICKS);
    }

    fn int1(&mut self, sensor_index: usize) {
        self.pulse(1 << sensor_index, NORMAL_MARKER_TICKS);
    }

    fn i2c_start(&mut self) {
        self.set(SDA_MASK, true);
        self.set(SCL_MASK, true);
        self.hold(1);
        self.set(SDA_MASK, false);
        self.hold(1);
    }

    fn i2c_repeated_start(&mut self) {
        self.set(SCL_MASK, false);
        self.set(SDA_MASK, true);
        self.hold(1);
        self.set(SCL_MASK, true);
        self.hold(1);
        self.set(SDA_MASK, false);
        self.hold(1);
    }

    fn i2c_stop(&mut self) {
        self.set(SCL_MASK, false);
        self.set(SDA_MASK, false);
        self.hold(1);
        self.set(SCL_MASK, true);
        self.hold(1);
        self.set(SDA_MASK, true);
        self.hold(1);
    }

    fn i2c_bit(&mut self, high: bool) {
        self.set(SCL_MASK, false);
        self.set(SDA_MASK, high);
        self.hold(1);
        self.set(SCL_MASK, true);
        self.hold(1);
        self.set(SCL_MASK, false);
    }

    fn i2c_byte(&mut self, byte: u8, ack_high: bool) {
        for shift in (0..8).rev() {
            self.i2c_bit(byte & (1 << shift) != 0);
        }
        self.i2c_bit(ack_high);
    }

    fn i2c_write(&mut self, address: u8, write: &[u8]) {
        self.i2c_start();
        self.i2c_byte(address << 1, false);
        for byte in write {
            self.i2c_byte(*byte, false);
        }
        self.i2c_stop();
    }

    fn i2c_read(&mut self, address: u8, read: &[u8]) {
        self.i2c_start();
        self.i2c_byte((address << 1) | 1, false);
        for (index, byte) in read.iter().enumerate() {
            self.i2c_byte(*byte, index + 1 == read.len());
        }
        self.i2c_stop();
    }

    fn i2c_write_read(&mut self, address: u8, write: &[u8], read: &[u8]) {
        self.i2c_start();
        self.i2c_byte(address << 1, false);
        for byte in write {
            self.i2c_byte(*byte, false);
        }
        self.i2c_repeated_start();
        self.i2c_byte((address << 1) | 1, false);
        for (index, byte) in read.iter().enumerate() {
            self.i2c_byte(*byte, index + 1 == read.len());
        }
        self.i2c_stop();
    }

    fn initialize_bank(&mut self, scenario: &Scenario) {
        if let Some(transcript) = &scenario.startup_transcript {
            self.append_startup_transcript(transcript);
            return;
        }
        let polls = scenario.startup_reset_polls.unwrap_or(1);
        let mut transfer_index = 0;
        for sensor_index in 0..scenario.startup_sensor_count.unwrap_or(SENSOR_COUNT) {
            let mask = 1_u8 << sensor_index;
            let mut transfers = vec![
                ShutdownTransfer {
                    address: 0x70,
                    write: vec![mask],
                    read: vec![],
                },
                ShutdownTransfer {
                    address: 0x70,
                    write: vec![],
                    read: vec![mask],
                },
                ShutdownTransfer {
                    address: 0x6a,
                    write: vec![0x12, 0x01],
                    read: vec![],
                },
            ];
            for poll in 0..polls {
                let value = if scenario.startup_reset_never_clears || poll + 1 < polls {
                    0x01
                } else {
                    0x44 // Only SW_RESET is constrained by the firmware's poll.
                };
                transfers.push(ShutdownTransfer {
                    address: 0x6a,
                    write: vec![0x12],
                    read: vec![value],
                });
            }
            transfers.push(ShutdownTransfer {
                address: 0x6a,
                write: vec![0x0f],
                read: vec![0x6a],
            });
            for (register, value) in [
                (0x12, 0x44),
                (0x10, 0x58),
                (0x11, 0x58),
                (0x0b, 0x80),
                (0x0d, u8::from(scenario.premature_int1_enable)),
            ] {
                transfers.push(ShutdownTransfer {
                    address: 0x6a,
                    write: vec![register, value],
                    read: vec![],
                });
                transfers.push(ShutdownTransfer {
                    address: 0x6a,
                    write: vec![register],
                    read: vec![value],
                });
            }
            for (sensor_step, transfer) in transfers.iter().enumerate() {
                if sensor_step == 1 {
                    self.hold(if scenario.startup_short_mux_settle {
                        0
                    } else {
                        20
                    });
                }
                if (4..3 + polls).contains(&sensor_step) {
                    self.hold(if scenario.startup_short_reset_delay {
                        0
                    } else {
                        200
                    });
                }
                if scenario.startup_int1 && sensor_step + 1 == transfers.len() {
                    // This branch has not yet validated its quiet profile.
                    self.int1(sensor_index);
                }
                if scenario.premature_snapshot && transfer_index == 2 {
                    self.pulse(D8_MASK, NORMAL_MARKER_TICKS);
                }
                if scenario.premature_commit && transfer_index == 2 {
                    self.pulse(D9_MASK, NORMAL_MARKER_TICKS);
                }
                self.append_startup_transfer(transfer, scenario.startup_mutation, transfer_index);
                transfer_index += 1;
            }
            if scenario.known_quiet_pulse_sensor == Some(sensor_index) {
                self.int1(sensor_index);
            }
        }
        for sensor_index in 0..scenario.startup_arm_sensor_count.unwrap_or(SENSOR_COUNT) {
            let mask = 1_u8 << sensor_index;
            let transfers = [
                ShutdownTransfer {
                    address: 0x70,
                    write: vec![mask],
                    read: vec![],
                },
                ShutdownTransfer {
                    address: 0x70,
                    write: vec![],
                    read: vec![mask],
                },
                ShutdownTransfer {
                    address: 0x6a,
                    write: vec![0x0d, 0x01],
                    read: vec![],
                },
                ShutdownTransfer {
                    address: 0x6a,
                    write: vec![0x0d],
                    read: vec![0x01],
                },
            ];
            for (sensor_step, transfer) in transfers.iter().enumerate() {
                if sensor_step == 1 {
                    self.hold(20);
                }
                if sensor_step == 2 && scenario.arm_int1_before_stop {
                    self.i2c_start();
                    self.i2c_byte(0x6a << 1, false);
                    self.i2c_byte(0x0d, false);
                    self.i2c_byte(0x01, false);
                    self.first_acquisition_edges[sensor_index] = Some(self.next_tick());
                    self.int1(sensor_index);
                    self.i2c_stop();
                } else {
                    self.append_startup_transfer(
                        transfer,
                        scenario.startup_mutation,
                        transfer_index,
                    );
                }
                transfer_index += 1;
            }
            if scenario.arm_extra_edge_before_first_mux && sensor_index == 0 {
                self.int1(0);
            }
        }
    }

    fn append_startup_transfer(
        &mut self,
        transfer: &ShutdownTransfer,
        mutation: Option<StartupMutation>,
        index: usize,
    ) {
        match mutation {
            Some(StartupMutation::Omit(target)) if target == index => {}
            Some(StartupMutation::Nack(target)) if target == index => {
                self.append_nacked_transfer(transfer);
            }
            Some(StartupMutation::Corrupt(target)) if target == index => {
                let mut corrupt = transfer.clone();
                if let Some(value) = corrupt.read.last_mut() {
                    *value ^= 1;
                } else {
                    *corrupt.write.last_mut().unwrap() ^= 1;
                }
                self.append_transfer(&corrupt);
            }
            _ => self.append_transfer(transfer),
        }
    }

    fn append_startup_transcript(&mut self, transcript: &str) {
        for line in transcript.lines() {
            let mut fields = line.split_whitespace();
            let fields = [
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ];
            let byte = |text: &str| u8::from_str_radix(text, 16).unwrap();
            match fields {
                [Some("D"), Some(delay), None, None, None] => {
                    self.hold(delay.parse::<u64>().unwrap() * 2);
                }
                [Some("W"), Some(address), Some(value), None, None] => {
                    self.i2c_write(byte(address), &[byte(value)]);
                }
                [Some("W"), Some(address), Some(register), Some(value), None] => {
                    self.i2c_write(byte(address), &[byte(register), byte(value)]);
                }
                [Some("R"), Some(address), Some(value), None, None] => {
                    self.i2c_read(byte(address), &[byte(value)]);
                }
                [Some("WR"), Some(address), Some(register), Some(value), None] => {
                    self.i2c_write_read(byte(address), &[byte(register)], &[byte(value)]);
                }
                _ => panic!("unexpected portable startup event: {line}"),
            }
        }
    }

    fn append_transfer(&mut self, transfer: &ShutdownTransfer) {
        if transfer.read.is_empty() {
            self.i2c_write(transfer.address, &transfer.write);
        } else if transfer.write.is_empty() {
            self.i2c_read(transfer.address, &transfer.read);
        } else {
            self.i2c_write_read(transfer.address, &transfer.write, &transfer.read);
        }
    }

    fn append_nacked_transfer(&mut self, transfer: &ShutdownTransfer) {
        self.i2c_start();
        let read = !transfer.read.is_empty() && transfer.write.is_empty();
        self.i2c_byte((transfer.address << 1) | u8::from(read), true);
        self.i2c_stop();
    }

    fn append_truncated_transfer(&mut self, transfer: &ShutdownTransfer) {
        self.i2c_start();
        let read = !transfer.read.is_empty() && transfer.write.is_empty();
        self.i2c_byte((transfer.address << 1) | u8::from(read), false);
    }

    fn append_shutdown(&mut self, mutation: Option<ShutdownMutation>) {
        let transfers = shutdown_transfers();
        let mut order = (0..SHUTDOWN_TRANSACTION_COUNT).collect::<Vec<_>>();
        if let Some(ShutdownMutation::Reorder(index)) = mutation {
            let peer = if index + 1 < SHUTDOWN_TRANSACTION_COUNT {
                index + 1
            } else {
                index - 1
            };
            order.swap(index, peer);
        }

        for transfer_index in order {
            if matches!(mutation, Some(ShutdownMutation::Omit(index)) if index == transfer_index) {
                continue;
            }
            let transfer = &transfers[transfer_index];
            if matches!(mutation, Some(ShutdownMutation::Nack(index)) if index == transfer_index) {
                self.append_nacked_transfer(transfer);
                continue;
            }
            if matches!(mutation, Some(ShutdownMutation::Truncate(index)) if index == transfer_index)
            {
                self.append_truncated_transfer(transfer);
                return;
            }
            if matches!(mutation, Some(ShutdownMutation::Corrupt(index)) if index == transfer_index)
            {
                let mut corrupt = transfer.clone();
                if let Some(byte) = corrupt.read.last_mut() {
                    *byte ^= 1;
                } else if let Some(byte) = corrupt.write.last_mut() {
                    *byte ^= 1;
                }
                self.append_transfer(&corrupt);
                continue;
            }
            self.append_transfer(transfer);
            if matches!(mutation, Some(ShutdownMutation::Duplicate(index)) if index == transfer_index)
            {
                self.append_transfer(transfer);
            }
        }
    }
}

#[derive(Clone, Debug)]
struct ShutdownTransfer {
    address: u8,
    write: Vec<u8>,
    read: Vec<u8>,
}

fn shutdown_transfers() -> Vec<ShutdownTransfer> {
    let mut transfers = Vec::with_capacity(SHUTDOWN_TRANSACTION_COUNT);
    transfers.push(ShutdownTransfer {
        address: 0x70,
        write: vec![0],
        read: vec![],
    });
    transfers.push(ShutdownTransfer {
        address: 0x70,
        write: vec![],
        read: vec![0],
    });
    for sensor_index in 0..SENSOR_COUNT {
        let mask = 1_u8 << sensor_index;
        transfers.push(ShutdownTransfer {
            address: 0x70,
            write: vec![mask],
            read: vec![],
        });
        transfers.push(ShutdownTransfer {
            address: 0x70,
            write: vec![],
            read: vec![mask],
        });
        for register in [0x0d, 0x10, 0x11] {
            transfers.push(ShutdownTransfer {
                address: 0x6a,
                write: vec![register, 0],
                read: vec![],
            });
            transfers.push(ShutdownTransfer {
                address: 0x6a,
                write: vec![register],
                read: vec![0],
            });
        }
    }
    transfers.push(ShutdownTransfer {
        address: 0x70,
        write: vec![0],
        read: vec![],
    });
    transfers.push(ShutdownTransfer {
        address: 0x70,
        write: vec![],
        read: vec![0],
    });
    assert_eq!(transfers.len(), SHUTDOWN_TRANSACTION_COUNT);
    transfers
}

fn manifest() -> AcquisitionManifest {
    let profile = CANONICAL_ACQUISITION_PROFILE;
    let sensors = std::array::from_fn(|sensor_index| SensorManifest {
        sensor_index: sensor_index as u8,
        mux_channel: sensor_index as u8,
        role: profile.sensor_order[sensor_index],
        mount_id: format!("left-mount-{sensor_index}"),
        wiring_id: format!("left-wiring-{sensor_index}"),
        interrupt_pin: format!("left-interrupt-{sensor_index}"),
        axis_map: [1, 0, 0, 0, 1, 0, 0, 0, 1],
    });
    let value = AcquisitionManifest {
        schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
        layout_id: CORE28_LAYOUT_ID.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        hands: vec![HandManifest {
            hand: ManifestHand::Left,
            controller_id: "gate0-left-controller".to_owned(),
            boot_id: "31".repeat(16),
            sensors,
        }],
        controller_board: "gate0-controller-board".to_owned(),
        firmware_build: "gate0-firmware-build".to_owned(),
        host_build: "gate0-host-build".to_owned(),
        sensor_model: "lsm6ds3tr-c".to_owned(),
        sensors_per_hand: 6,
        sensor_odr_hz: profile.sensor_odr_hz,
        accelerometer_range_g: profile.accelerometer_range_g,
        gyroscope_range_dps: profile.gyroscope_range_dps,
        filter_profile: "gate0-filter".to_owned(),
        i2c_bus_hz: profile.i2c_bus_hz,
        nominal_period_us: profile.nominal_period_us,
        prompt_corpus_sha256: "aa".repeat(32),
        study_id: "gate0-study".to_owned(),
        split_id: "gate0-split".to_owned(),
        participant_alias: "gate0-participant".to_owned(),
        consent_record: "gate0-consent".to_owned(),
        retention_policy: "gate0-retention".to_owned(),
        rig_evidence_sha256: format_sha256(&[0xbb; 32]),
        axis_characterization_sha256: format_sha256(&[0xcc; 32]),
        calibration_artifact_sha256: None,
        calibration_validation_sha256: None,
    };
    value.validate().unwrap();
    value
}

fn motion_reading(sequence: u32, sensor_index: usize) -> SensorReading {
    let sequence_bytes = sequence.to_le_bytes();
    let sequence_low = i16::from_le_bytes([sequence_bytes[0], sequence_bytes[1]]);
    let sequence_high = i16::from_le_bytes([sequence_bytes[2], sequence_bytes[3]]);
    let sensor = sensor_index as i16;
    SensorReading {
        acceleration: [sensor, sensor + 1, sensor + 2],
        angular_velocity: [sequence_low, sequence_high, sensor],
        sample_time_offset_us: 0,
    }
}

fn motion_bytes(sequence: u32, sensor_index: usize) -> [u8; 12] {
    let mut bytes = [0_u8; 12];
    for (index, value) in motion_reading(sequence, sensor_index)
        .angular_velocity
        .into_iter()
        .chain(motion_reading(sequence, sensor_index).acceleration)
        .enumerate()
    {
        bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn frame(manifest: &AcquisitionManifest, sequence: u32) -> SensorFrame {
    let identity = manifest.frame_identities().unwrap()[0];
    SensorFrame {
        hand: Hand::Left,
        controller_id_sha256: identity.controller_id_sha256,
        boot_id: identity.boot_id,
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence,
        device_time_us: u64::from(sequence) * 5_000,
        sensors: std::array::from_fn(|sensor_index| motion_reading(sequence, sensor_index)),
    }
}

fn finalized_session(
    manifest: &AcquisitionManifest,
    sequences: &[u32],
    wrong_payload: bool,
    first_offsets: Option<[i16; SENSOR_COUNT]>,
) -> FinalizedSession {
    let header = SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256: manifest.sha256().unwrap(),
        session_id: [0x47; 16],
    };
    let mut writer = SessionWriter::new(Vec::new(), header).unwrap();
    let (&final_sequence, ordinary_sequences) = sequences
        .split_last()
        .expect("Gate 0 fixture needs a final frame");
    let fixture_frame = |sequence| {
        let mut frame = frame(manifest, sequence);
        if let Some(offsets) = first_offsets {
            // Early-enabled sensors retain their 208 Hz phases in every frame,
            // independently of the 200 Hz aggregate clock.
            let phase_change_us = i128::from(sequence) * i128::from(NOMINAL_EDGE_INTERVAL_TICKS)
                / 2
                - i128::from(sequence) * i128::from(manifest.nominal_period_us);
            frame.device_time_us += 100_000;
            for (sensor, offset) in frame.sensors.iter_mut().zip(offsets) {
                sensor.sample_time_offset_us =
                    i16::try_from(i128::from(offset) + phase_change_us).unwrap();
            }
        }
        if wrong_payload && sequence == 1 {
            frame.sensors[0].angular_velocity[0] ^= 0x55;
        }
        frame
    };
    for sequence in ordinary_sequences {
        let frame = fixture_frame(*sequence);
        writer
            .append_frame(
                ControllerStream::Left,
                &frame,
                u64::from(*sequence) * 5_000_000,
            )
            .unwrap();
    }
    let identity = manifest.frame_identities().unwrap()[0];
    let request_start_time_ns = 100_000_000;
    let request = QuiesceRequest {
        hand: identity.hand,
        controller_id_sha256: identity.controller_id_sha256,
        boot_id: identity.boot_id,
        session_id: header.session_id,
        request_start_time_ns,
    };
    writer
        .append_quiesce_request(ControllerStream::Left, &request, request_start_time_ns)
        .unwrap();
    let final_frame = fixture_frame(final_sequence);
    writer
        .append_frame(
            ControllerStream::Left,
            &final_frame,
            request_start_time_ns + 5_000_000,
        )
        .unwrap();
    writer
        .append_quiesce_receipt(
            ControllerStream::Left,
            &QuiesceReceipt {
                hand: identity.hand,
                controller_id_sha256: identity.controller_id_sha256,
                boot_id: identity.boot_id,
                session_id: header.session_id,
                request_start_time_ns,
                final_sequence,
            },
            request_start_time_ns + 10_000_000,
        )
        .unwrap();
    writer.finish_finalized().unwrap()
}

fn append_sensor_read(
    wave: &mut Waveform,
    sequence: u32,
    sensor_index: usize,
    scenario: &Scenario,
) {
    let mux_mask = if scenario.wrong_mux && sequence == 0 && sensor_index == 0 {
        1 << 1
    } else {
        1 << sensor_index
    };
    wave.i2c_write(0x70, &[mux_mask]);
    wave.i2c_read(0x70, &[mux_mask]);

    let statuses: &[u8] = if scenario.ready_before_final && sequence == 0 && sensor_index == 0 {
        &[0x03, 0x00]
    } else if scenario.four_status_polls {
        &[0x00, 0x00, 0x00, 0x03]
    } else {
        &[0x03]
    };
    for status in statuses {
        wave.i2c_write_read(0x6a, &[0x1e], &[*status]);
    }

    let mut motion = motion_bytes(sequence, sensor_index);
    if scenario.wrong_raw_payload && sequence == 1 && sensor_index == 0 {
        motion[0] ^= 0x55;
    }
    wave.i2c_write_read(0x6a, &[0x22], &motion);
}

fn build_waveform(scenario: &Scenario, final_sequence: u32) -> Waveform {
    let mut wave = Waveform::new();
    wave.boot_anchor(scenario);
    wave.hold(SENSOR_STARTUP_DELAY_TICKS);
    wave.initialize_bank(scenario);
    wave.hold(scenario.priming_idle_ticks.unwrap_or(0));
    if scenario.ambiguous_bus_transition {
        wave.set_state(wave.state & !(SDA_MASK | SCL_MASK));
        wave.set(SCL_MASK, true);
        wave.set(SDA_MASK, true);
        wave.hold(4);
    }
    if scenario.malformed_i2c {
        wave.i2c_start();
        wave.i2c_stop();
    }
    if scenario.misordered_d9 {
        wave.pulse(D9_MASK, NORMAL_MARKER_TICKS);
    }
    if scenario.marker_bus_transition_overlap {
        wave.set_state((wave.state & !SDA_MASK) | D9_MASK);
        wave.set_state((wave.state | SDA_MASK) & !D9_MASK);
        wave.hold(4);
    }
    if scenario.many_reserved_transitions {
        for _ in 0..(MAX_GATE0_TRACE_VIOLATION_WITNESSES + 8) {
            wave.set(RESERVED_MASK, true);
            wave.set(RESERVED_MASK, false);
        }
    }

    let first_frame_tick = wave.next_tick();
    for sequence in 0..=final_sequence {
        if sequence > 0 && !scenario.arm_int1_before_stop {
            let elapsed = if scenario.jitter_average_hides_outliers {
                match sequence {
                    1 => 9_000,
                    2 => 2 * NOMINAL_EDGE_INTERVAL_TICKS,
                    _ => unreachable!(),
                }
            } else if scenario.slow_odr {
                NOMINAL_EDGE_INTERVAL_TICKS + 3_000
            } else {
                NOMINAL_EDGE_INTERVAL_TICKS
            };
            let elapsed =
                elapsed + u64::from(scenario.pre_snapshot_decimation) * NOMINAL_EDGE_INTERVAL_TICKS;
            wave.hold_until(first_frame_tick + elapsed);
        }
        for sensor_index in 0..SENSOR_COUNT {
            if scenario.missing_int1 && sequence == 0 && sensor_index == 0 {
                continue;
            }
            if sequence == 0 && wave.first_acquisition_edges[sensor_index].is_some() {
                continue;
            }
            if sequence > 0 && scenario.arm_int1_before_stop {
                wave.hold_until(
                    wave.first_acquisition_edges[sensor_index].unwrap()
                        + u64::from(sequence) * NOMINAL_EDGE_INTERVAL_TICKS
                        - 1,
                );
            }
            if sequence == 0 {
                wave.first_acquisition_edges[sensor_index] = Some(wave.next_tick());
            }
            wave.int1(sensor_index);
        }
        if scenario.next_edge_crossing && sequence == 0 {
            wave.int1(0);
        }
        for sensor_index in 0..SENSOR_COUNT {
            append_sensor_read(&mut wave, sequence, sensor_index, scenario);
            if scenario.missing_d9 && sequence == 0 && sensor_index == 0 {
                continue;
            }
            if scenario.d8_during_d9 && sequence == 0 && sensor_index == 0 {
                wave.set(D9_MASK, true);
                wave.pulse(D8_MASK, NORMAL_MARKER_TICKS);
                wave.set(D9_MASK, false);
            } else {
                let width = if scenario.short_normal_marker && sequence == 0 && sensor_index == 0 {
                    1
                } else {
                    NORMAL_MARKER_TICKS
                };
                wave.pulse(D9_MASK, width);
            }
        }
        if scenario.pre_snapshot_decimation && sequence == 0 {
            wave.hold_until(first_frame_tick + NOMINAL_EDGE_INTERVAL_TICKS - 1);
            for sensor_index in 0..SENSOR_COUNT {
                wave.int1(sensor_index);
            }
            if scenario.pre_snapshot_extra_edge {
                wave.int1(0);
            }
            for sensor_index in 0..SENSOR_COUNT {
                append_sensor_read(&mut wave, sequence, sensor_index, scenario);
                wave.pulse(D9_MASK, NORMAL_MARKER_TICKS);
            }
        }
        // Put the replacement in a fully anchored aggregate interval so its
        // phase is measured from the preceding D8 edge, like decisive data.
        if scenario.decimation && sequence == 1 {
            wave.int1(0);
            append_sensor_read(&mut wave, sequence, 0, scenario);
            wave.pulse(D9_MASK, NORMAL_MARKER_TICKS);
        }
        if sequence == final_sequence && scenario.pre_cut_unmatched_edge {
            wave.int1(0);
        }
        if sequence == 0 {
            wave.first_snapshot_tick = Some(wave.next_tick());
        }
        if sequence == final_sequence && scenario.int1_at_final_cut {
            wave.set_state(wave.state | D8_MASK | 1);
            wave.hold(NORMAL_MARKER_TICKS - 1);
            wave.set_state(wave.state & !(D8_MASK | 1));
        } else {
            wave.pulse(D8_MASK, NORMAL_MARKER_TICKS);
        }
    }
    if scenario.post_cut_int1 {
        wave.int1(0);
    }
    if scenario.extra_acquisition_after_final {
        wave.int1(0);
        append_sensor_read(&mut wave, final_sequence.wrapping_add(1), 0, scenario);
        wave.pulse(D9_MASK, NORMAL_MARKER_TICKS);
        wave.pulse(D8_MASK, NORMAL_MARKER_TICKS);
    }
    wave.append_shutdown(scenario.shutdown_mutation);
    if scenario.premarker_interrupt {
        wave.hold(TERMINAL_PREMARKER_IDLE_TICKS);
        wave.int1(0);
    }
    let premarker_idle = scenario
        .premarker_idle_ticks
        .unwrap_or(TERMINAL_PREMARKER_IDLE_TICKS);
    // The final STOP transition itself is the first 0x0300 sample. Add only
    // the remaining samples needed before the terminal marker rise. I2C STOP
    // already holds one extra idle sample; an INT1 fall does not.
    let existing_idle_samples = if scenario.premarker_interrupt { 1 } else { 2 };
    wave.hold(premarker_idle.saturating_sub(existing_idle_samples));
    let marker_width = scenario
        .terminal_marker_width_ticks
        .unwrap_or(TERMINAL_MARKER_TICKS);
    if scenario.staggered_terminal_marker {
        wave.set(D8_MASK, true);
        wave.set(D9_MASK, true);
        wave.hold(marker_width.saturating_sub(1));
        wave.set(D8_MASK, false);
        wave.set(D9_MASK, false);
    } else {
        wave.pulse(D8_MASK | D9_MASK, marker_width);
    }
    if scenario.duplicate_terminal_marker {
        wave.pulse(D8_MASK | D9_MASK, marker_width);
    }
    if let Some(mask) = scenario.post_marker_mask {
        wave.pulse(mask, NORMAL_MARKER_TICKS);
    }
    let terminal_idle_samples = scenario.terminal_idle_samples.unwrap_or(16);
    wave.hold(terminal_idle_samples.saturating_sub(1));
    wave
}

fn temp_dir() -> PathBuf {
    let root = env::var_os("TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = root.join(format!("gate0-trace-{}-{sequence}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn write_fixture(
    scenario: &Scenario,
    session_sequences: &[u32],
    wrong_session_payload: bool,
) -> Fixture {
    let directory = temp_dir();
    let manifest = manifest();
    let final_sequence = *session_sequences
        .last()
        .expect("Gate 0 fixture has a final frame");
    let wave = build_waveform(scenario, final_sequence);
    let first_offsets = scenario.arm_int1_before_stop.then(|| {
        let snapshot = wave.first_snapshot_tick.unwrap() as i64;
        wave.first_acquisition_edges
            .map(|edge| i16::try_from((edge.unwrap() as i64 - snapshot) / 2).unwrap())
    });
    let session = finalized_session(
        &manifest,
        session_sequences,
        wrong_session_payload,
        first_offsets,
    );
    let words = &wave.words;
    let raw_bytes = words
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let raw_capture = directory.join("capture.raw");
    fs::write(&raw_capture, &raw_bytes).unwrap();

    let capture_binary = directory.join("capture-tool");
    let capture_binary_bytes = b"static patched sigrok fixture";
    fs::write(&capture_binary, capture_binary_bytes).unwrap();
    let firmware_bundle = directory.join("firmware.bundle");
    let firmware_bytes = b"deterministic DSLogic firmware fixture bundle";
    fs::write(&firmware_bundle, firmware_bytes).unwrap();

    let capture_log = directory.join("capture.log");
    let firmware_hash = if scenario.wrong_firmware_log_hash {
        "00".repeat(32)
    } else {
        format_sha256(&sha256(firmware_bytes))
    };
    let summary = wrapper_capture_summary(
        words.len(),
        1,
        STOP_WINDOW_FIRST_BYTE,
        session.bytes().len(),
        &manifest,
        &firmware_hash,
    );
    let summary_terminator = if scenario.unterminated_capture_summary {
        ""
    } else {
        "\n"
    };
    let driver_transfers = if scenario.mismatched_driver_receipt {
        2
    } else {
        1
    };
    let driver_receipt_suffix = if scenario.trailing_driver_receipt_token {
        " injected=1"
    } else {
        ""
    };
    let provenance_log = format!(
        concat!(
            "IKB_DSLOGIC_TRANSFER_V1 completed_transfers={} anomalies=0{}\n",
            "IKB_GATE0_FIRMWARE_V1 name=dreamsourcelab-dslogic-plus-fpga.fw bytes=341160 sha256=e16c70e4af9181044e3ec3a363e4bb109f1d731b4f9f046f8bc1d1015a7842a7\n",
            "IKB_GATE0_FIRMWARE_V1 name=dreamsourcelab-dslogic-plus-fx2.fw bytes=8120 sha256=9197e08dd33a9cda61cc8cd3de559cbae9b049b2dfa827c089ffe0528f5dcc5e\n",
        ),
        driver_transfers, driver_receipt_suffix,
    );
    let trailing_log = if scenario.summary_not_final {
        "trailing output\n"
    } else {
        ""
    };
    let log = if scenario.oversized_capture_log_line {
        format!("{}\n", "x".repeat(4_097))
    } else if scenario.bad_capture_log {
        format!("usb transfer failed\n{provenance_log}{summary}\n")
    } else {
        format!("{provenance_log}{summary}{summary_terminator}{trailing_log}")
    };
    fs::write(&capture_log, log.as_bytes()).unwrap();

    let sidecar = directory.join("capture.json");
    let sidecar_json = json!({
        "schema": GATE0_CAPTURE_SIDECAR_SCHEMA,
        "raw_capture_sha256": format_sha256(&sha256(&raw_bytes)),
        "raw_capture_bytes": raw_bytes.len(),
        "word_bytes": 2,
        "byte_order": "little",
        "sample_rate_hz": GATE0_SAMPLE_RATE_HZ,
        "channel_map": {
            "int1": [0, 1, 2, 3, 4, 5],
            "d8": 6,
            "d9": 7,
            "sda": 8,
            "scl": 9
        },
        "voltage_threshold_low_mv": 1_400,
        "voltage_threshold_high_mv": 3_600,
        "requested_samples": words.len(),
        "capture_started_unix_ms": 1_000,
        "capture_finished_unix_ms": 3_611_000,
        "capture_binary": bound_artifact("capture-tool", capture_binary_bytes),
        "capture_log": bound_artifact("capture.log", log.as_bytes()),
        "device_firmware_bundle": bound_artifact("firmware.bundle", firmware_bytes),
        "driver_base_commit": "0bc2487778e660f4d3116729b6f4aee2b1996bb0",
        "sigrok_cli_base_commit": "f44dd91347e7ac797cefc23162b9fcf0b7329f1f",
        "driver_patch_sha256": format_sha256(&sha256(b"fail-closed-driver-patch")),
        "device_model": "DSLogic Plus",
        "capture_exit_status": 0,
        "command": [
            ".capture-tool.partial",
            "--driver", "dreamsourcelab-dslogic",
            "--channels", "0,1,2,3,4,5,6,7,8,9",
            "--config", "samplerate=2m:voltage_threshold=1.4-3.6:continuous=true",
            "--samples", words.len().to_string(),
            "--output-format", "binary",
            "--output-file", ".capture.raw.partial",
            "--loglevel", "4"
        ]
    });
    fs::write(&sidecar, serde_json::to_vec(&sidecar_json).unwrap()).unwrap();

    Fixture {
        manifest,
        session,
        raw_capture,
        sidecar,
        capture_binary,
        firmware_bundle,
    }
}

fn bound_artifact(path: &str, bytes: &[u8]) -> serde_json::Value {
    json!({
        "path": path,
        "sha256": format_sha256(&sha256(bytes)),
        "bytes": bytes.len()
    })
}

fn wrapper_capture_summary(
    requested_samples: usize,
    completed_transfers: u64,
    stop_observed_raw_bytes: u64,
    session_bytes: usize,
    manifest: &AcquisitionManifest,
    firmware_bundle_sha256: &str,
) -> String {
    format!(
        concat!(
            "IKB_GATE0_CAPTURE_V2 samples={} completed_transfers={} anomalies=0 ",
            "stop_observed_raw_bytes={} session_bytes={} locked_manifest_sha256={} ",
            "firmware_bundle_sha256={}"
        ),
        requested_samples,
        completed_transfers,
        stop_observed_raw_bytes,
        session_bytes,
        format_sha256(&manifest.sha256().unwrap()),
        firmware_bundle_sha256,
    )
}

fn rebind_capture_log(fixture: &Fixture, log: &str) {
    let capture_log = fixture.sidecar.with_file_name("capture.log");
    fs::write(capture_log, log.as_bytes()).unwrap();
    let mut sidecar: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.sidecar).unwrap()).unwrap();
    sidecar["capture_log"]["bytes"] = json!(log.len());
    sidecar["capture_log"]["sha256"] = json!(format_sha256(&sha256(log.as_bytes())));
    fs::write(&fixture.sidecar, serde_json::to_vec(&sidecar).unwrap()).unwrap();
}

fn has_violation(evidence: &Gate0TraceEvidence, code: Gate0TraceViolationCode) -> bool {
    evidence
        .witnesses()
        .violations
        .iter()
        .any(|violation| violation.code == code)
}

fn unexpected_baseline_violations(evidence: &Gate0TraceEvidence) -> Vec<Gate0TraceViolationCode> {
    evidence
        .witnesses()
        .violations
        .iter()
        .map(|violation| violation.code)
        .filter(|code| {
            !matches!(
                code,
                Gate0TraceViolationCode::CapturePlanMismatch
                    | Gate0TraceViolationCode::CaptureTooShort
                    | Gate0TraceViolationCode::TerminalIdleTooShort
            )
        })
        .collect()
}

fn has_serialized_violation(evidence: &Gate0TraceEvidence, code: &str) -> bool {
    evidence
        .witnesses()
        .violations
        .iter()
        .any(|violation| serde_json::to_value(violation.code).unwrap() == json!(code))
}

#[test]
fn portable_c_startup_transcript_is_accepted_by_raw_reducer() {
    let root = env::var_os("TEST_SRCDIR").expect("Bazel supplies its runfiles root");
    let relative = env::var("IKB_STARTUP_FIXTURE_RLOCATION")
        .expect("Bazel supplies the portable startup fixture path");
    let output = Command::new(PathBuf::from(root).join(relative))
        .arg("--startup-transcript")
        .output()
        .expect("portable C startup fixture starts");
    assert!(output.status.success(), "{:?}", output.stderr);
    let transcript = String::from_utf8(output.stdout).expect("startup transcript is UTF-8");
    assert!(!transcript.is_empty());
    let fixture = write_fixture(
        &Scenario {
            startup_transcript: Some(transcript),
            ..Scenario::default()
        },
        &[0, 1],
        false,
    );
    let evidence = fixture.reduce().unwrap();
    assert_eq!(unexpected_baseline_violations(&evidence), []);
    let metrics = serde_json::to_value(evidence.metrics()).unwrap();
    assert_eq!(metrics["startup_transactions"], 114);
    assert!(metrics["startup_end_sample"].as_u64().is_some());
    assert_eq!(evidence.metrics().linked_reads, [2; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().shutdown_transactions, 52);
}

#[test]
fn pre_snapshot_replacements_are_accounted_without_inventing_a_phase_grid() {
    let fixture = write_fixture(
        &Scenario {
            pre_snapshot_decimation: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    );
    let evidence = fixture.reduce().unwrap();
    assert_eq!(unexpected_baseline_violations(&evidence), []);
    assert_eq!(evidence.metrics().int1_edges, [3; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().committed_reads, [3; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().selected_reads, [2; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().decimated_reads, [1; SENSOR_COUNT]);
    assert_eq!(
        evidence
            .metrics()
            .phase_histogram
            .iter()
            .flatten()
            .sum::<u64>(),
        0
    );
    let metrics = serde_json::to_value(evidence.metrics()).unwrap();
    assert_eq!(
        metrics["pre_snapshot_decimated_reads"],
        json!([1, 1, 1, 1, 1, 1])
    );
}

#[test]
fn pre_grid_and_grid_replacements_keep_complete_read_accounting() {
    let fixture = write_fixture(
        &Scenario {
            pre_snapshot_decimation: true,
            decimation: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    );
    let evidence = fixture.reduce().unwrap();
    let metrics = serde_json::to_value(evidence.metrics()).unwrap();
    assert_eq!(
        metrics["pre_snapshot_decimated_reads"],
        json!([1, 1, 1, 1, 1, 1])
    );
    assert_eq!(
        evidence
            .metrics()
            .phase_histogram
            .iter()
            .flatten()
            .sum::<u64>(),
        1
    );
    assert_eq!(evidence.metrics().decimated_reads.iter().sum::<u64>(), 7);
    assert!(!has_violation(
        &evidence,
        Gate0TraceViolationCode::AccountingMismatch
    ));
    assert!(has_violation(
        &evidence,
        Gate0TraceViolationCode::OdrOutOfProvisionalBand
    ));
}

#[test]
fn pre_snapshot_replacement_keeps_pending_edge_checks_strict() {
    let fixture = write_fixture(
        &Scenario {
            pre_snapshot_decimation: true,
            pre_snapshot_extra_edge: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    );
    let evidence = fixture.reduce().unwrap();
    assert!(has_violation(
        &evidence,
        Gate0TraceViolationCode::NextEdgeCrossing
    ));
    assert!(has_violation(
        &evidence,
        Gate0TraceViolationCode::AccountingMismatch
    ));
}

#[test]
fn first_clean_cache_must_arrive_within_priming_cap() {
    for (idle_ticks, timeout) in [(180_000, false), (200_000, true)] {
        let fixture = write_fixture(
            &Scenario {
                priming_idle_ticks: Some(idle_ticks),
                ..Scenario::default()
            },
            &[0, 1],
            false,
        );
        let evidence = fixture.reduce().unwrap();
        assert_eq!(
            has_serialized_violation(&evidence, "startup_priming_timeout"),
            timeout
        );
        if !timeout {
            assert_eq!(unexpected_baseline_violations(&evidence), []);
        }
    }
}

#[test]
fn int1_during_enable_write_is_admitted_before_stop() {
    let fixture = write_fixture(
        &Scenario {
            arm_int1_before_stop: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    );
    let evidence = fixture.reduce().unwrap();
    assert_eq!(unexpected_baseline_violations(&evidence), []);
    assert_eq!(evidence.metrics().int1_edges, [2; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().committed_reads, [2; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().linked_reads, [2; SENSOR_COUNT]);
}

#[test]
fn admitted_arming_edge_cannot_be_forgiven_before_first_mux() {
    let fixture = write_fixture(
        &Scenario {
            arm_int1_before_stop: true,
            arm_extra_edge_before_first_mux: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    );
    let evidence = fixture.reduce().unwrap();
    assert!(has_violation(
        &evidence,
        Gate0TraceViolationCode::NextEdgeCrossing
    ));
    assert!(has_violation(
        &evidence,
        Gate0TraceViolationCode::AccountingMismatch
    ));
}

#[test]
fn incomplete_arming_cannot_qualify() {
    for armed_sensors in 0..SENSOR_COUNT {
        let fixture = write_fixture(
            &Scenario {
                startup_arm_sensor_count: Some(armed_sensors),
                ..Scenario::default()
            },
            &[0, 1],
            false,
        );
        let evidence = fixture.reduce().unwrap();
        assert!(has_serialized_violation(&evidence, "startup_incomplete"));
        assert!(has_serialized_violation(
            &evidence,
            "startup_transaction_mismatch"
        ));
    }
}

#[test]
fn selected_firmware_boot_initializes_every_branch_before_acquisition() {
    for polls in [1, 2, 100] {
        let fixture = write_fixture(
            &Scenario {
                startup_reset_polls: Some(polls),
                startup_int1: true,
                ..Scenario::default()
            },
            &[0, 1],
            false,
        );
        let evidence = fixture.reduce().unwrap();
        assert_eq!(
            unexpected_baseline_violations(&evidence),
            [],
            "polls={polls}"
        );
        assert_eq!(evidence.metrics().int1_edges, [2; SENSOR_COUNT]);
        assert_eq!(evidence.metrics().linked_reads, [2; SENSOR_COUNT]);
        assert_eq!(evidence.metrics().shutdown_transactions, 52);
        let metrics = serde_json::to_value(evidence.metrics()).unwrap();
        assert_eq!(
            metrics["startup_unadmitted_int1_edges"],
            json!([1, 1, 1, 1, 1, 1])
        );
    }
}

#[test]
fn known_quiet_sensor_pulse_before_enable_start_cannot_qualify() {
    for sensor_index in 0..SENSOR_COUNT {
        let fixture = write_fixture(
            &Scenario {
                known_quiet_pulse_sensor: Some(sensor_index),
                ..Scenario::default()
            },
            &[0, 1],
            false,
        );
        let evidence = fixture.reduce().unwrap();
        assert!(has_serialized_violation(
            &evidence,
            "startup_known_quiet_int1"
        ));
        assert_eq!(evidence.metrics().int1_edges, [2; SENSOR_COUNT]);
    }
}

#[test]
fn configuration_keeps_int1_quiet_until_callbacks_are_installed() {
    let fixture = write_fixture(
        &Scenario {
            premature_int1_enable: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    );
    let evidence = fixture.reduce().unwrap();
    assert!(has_serialized_violation(
        &evidence,
        "startup_transaction_mismatch"
    ));
    assert!(has_serialized_violation(&evidence, "startup_incomplete"));
}

#[test]
fn startup_missing_wrong_or_nacked_transfers_cannot_qualify() {
    for index in 0..114 {
        for mutation in [
            StartupMutation::Omit(index),
            StartupMutation::Corrupt(index),
            StartupMutation::Nack(index),
        ] {
            let fixture = write_fixture(
                &Scenario {
                    startup_mutation: Some(mutation),
                    ..Scenario::default()
                },
                &[0, 1],
                false,
            );
            let evidence = fixture.reduce().unwrap();
            assert!(
                has_serialized_violation(&evidence, "startup_transaction_mismatch"),
                "{mutation:?}",
            );
            assert!(has_serialized_violation(&evidence, "startup_incomplete"));
        }
    }
    for sensor_count in 0..SENSOR_COUNT {
        let fixture = write_fixture(
            &Scenario {
                startup_sensor_count: Some(sensor_count),
                ..Scenario::default()
            },
            &[0, 1],
            false,
        );
        assert!(has_serialized_violation(
            &fixture.reduce().unwrap(),
            "startup_incomplete"
        ));
    }
}

#[test]
fn startup_reset_must_clear_within_firmware_poll_limit() {
    for (polls, never_clears) in [(0, false), (100, true), (101, false)] {
        let fixture = write_fixture(
            &Scenario {
                startup_reset_polls: Some(polls),
                startup_reset_never_clears: never_clears,
                ..Scenario::default()
            },
            &[0, 1],
            false,
        );
        let evidence = fixture.reduce().unwrap();
        let code = if polls == 0 {
            "startup_transaction_mismatch"
        } else {
            "startup_reset_poll_limit"
        };
        assert!(has_serialized_violation(&evidence, code));
        assert!(has_serialized_violation(&evidence, "startup_incomplete"));
    }
}

#[test]
fn startup_rejects_premature_snapshot_or_commit() {
    for (premature_snapshot, premature_commit) in [(true, false), (false, true)] {
        let fixture = write_fixture(
            &Scenario {
                premature_snapshot,
                premature_commit,
                ..Scenario::default()
            },
            &[0, 1],
            false,
        );
        let evidence = fixture.reduce().unwrap();
        assert!(has_serialized_violation(
            &evidence,
            "startup_premature_acquisition"
        ));
    }
}

#[test]
fn startup_honors_mux_settle_and_busy_reset_retry_delays() {
    for (startup_short_mux_settle, startup_short_reset_delay) in [(true, false), (false, true)] {
        let fixture = write_fixture(
            &Scenario {
                startup_reset_polls: Some(2),
                startup_short_mux_settle,
                startup_short_reset_delay,
                ..Scenario::default()
            },
            &[0, 1],
            false,
        );
        let evidence = fixture.reduce().unwrap();
        assert!(has_serialized_violation(
            &evidence,
            "startup_transaction_mismatch"
        ));
    }
}

#[test]
fn raw_waveform_derives_every_read_and_links_host_frames() {
    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    let evidence = fixture.reduce().expect("raw fixture reduces");

    assert_eq!(unexpected_baseline_violations(&evidence), []);
    assert_eq!(evidence.metrics().decoded_i2c_transactions, 214);
    assert_eq!(evidence.metrics().int1_edges, [2; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().completed_reads, [2; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().committed_reads, [2; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().selected_reads, [2; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().linked_reads, [2; SENSOR_COUNT]);
    assert_eq!(evidence.metrics().snapshots, 2);
    assert_eq!(evidence.metrics().final_sequence, 1);
    assert_eq!(evidence.metrics().shutdown_transactions, 52);
    assert_eq!(evidence.metrics().shutdown_scl_rising_edges, 1_422);
    assert_eq!(evidence.metrics().terminal_marker_width_samples, Some(300));
    assert_eq!(evidence.metrics().max_pending_reads, 6);
    assert!(evidence.metrics().max_read_latency_ticks > 0);
    assert!(evidence.metrics().max_i2c_to_d9_commit_ticks > 0);
    assert!(evidence.witnesses().boot_anchor_end.is_some());

    let report = score_gate0a(&fixture.manifest, &fixture.session, &evidence).unwrap();
    assert_eq!(
        report.criteria().external_evidence_binding,
        Gate0Status::Pass
    );
    assert_eq!(report.criteria().logic_analyzer_timing, Gate0Status::Fail);
    assert_eq!(report.trace(), &evidence);
}

#[test]
fn host_sequence_gap_is_classified_without_payload_uniqueness() {
    let fixture = write_fixture(&Scenario::default(), &[1], false);
    let evidence = fixture.reduce().unwrap();

    assert_eq!(evidence.metrics().host_dropped_snapshots, 1);
    assert_eq!(evidence.metrics().linked_snapshots, 1);
    assert_eq!(evidence.metrics().host_dropped_reads, [1; SENSOR_COUNT]);
    assert_eq!(unexpected_baseline_violations(&evidence), []);

    let report = score_gate0a(&fixture.manifest, &fixture.session, &evidence).unwrap();
    assert_eq!(report.metrics().accounted_frame_opportunities, 2);
    assert_eq!(report.criteria().valid_receipt, Gate0Status::Fail);
}

#[test]
fn wrapper_v2_summary_reaches_the_production_reducer_and_scorer() {
    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    let capture_log = fs::read_to_string(fixture.sidecar.with_file_name("capture.log")).unwrap();
    let firmware_hash = format_sha256(&sha256(&fs::read(&fixture.firmware_bundle).unwrap()));
    let expected_summary = wrapper_capture_summary(
        fs::metadata(&fixture.raw_capture).unwrap().len() as usize / 2,
        1,
        STOP_WINDOW_FIRST_BYTE,
        fixture.session.bytes().len(),
        &fixture.manifest,
        &firmware_hash,
    );
    assert_eq!(capture_log.lines().last(), Some(expected_summary.as_str()));

    let evidence = fixture.reduce().expect("wrapper-shaped V2 log reduces");
    let report = score_gate0a(&fixture.manifest, &fixture.session, &evidence)
        .expect("production scorer accepts reduced V2 evidence");
    let report_json: serde_json::Value =
        serde_json::from_slice(&report.canonical_json().unwrap()).unwrap();
    assert_eq!(report_json["schema"], GATE0A_REPORT_SCHEMA);
    assert_eq!(report_json["criteria"]["external_evidence_binding"], "pass");
}

#[test]
fn legacy_v1_capture_summary_is_rejected_after_hash_binding() {
    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    let capture_log_path = fixture.sidecar.with_file_name("capture.log");
    let capture_log = fs::read_to_string(capture_log_path).unwrap();
    let summary_offset = capture_log.rfind("IKB_GATE0_CAPTURE_V2 ").unwrap();
    let requested_samples = fs::metadata(&fixture.raw_capture).unwrap().len() / 2;
    let firmware_hash = format_sha256(&sha256(&fs::read(&fixture.firmware_bundle).unwrap()));
    let legacy_log = format!(
        "{}IKB_GATE0_CAPTURE_V1 samples={} completed_transfers=1 anomalies=0 firmware_bundle_sha256={}\n",
        &capture_log[..summary_offset],
        requested_samples,
        firmware_hash
    );
    rebind_capture_log(&fixture, &legacy_log);

    assert!(matches!(
        fixture.reduce(),
        Err(Gate0TraceError::CaptureLogRejected(_))
    ));
}

#[test]
fn v2_summary_enforces_terminal_bindings_stop_window_and_field_order() {
    let reference = write_fixture(&Scenario::default(), &[0, 1], false);
    let session_bytes = reference.session.bytes().len();
    let manifest_hash = format_sha256(&reference.manifest.sha256().unwrap());
    let invalid_replacements = [
        (
            format!("stop_observed_raw_bytes={STOP_WINDOW_FIRST_BYTE}"),
            format!("stop_observed_raw_bytes={}", STOP_WINDOW_FIRST_BYTE - 1),
        ),
        (
            format!("stop_observed_raw_bytes={STOP_WINDOW_FIRST_BYTE}"),
            format!("stop_observed_raw_bytes={}", STOP_WINDOW_LAST_BYTE + 1),
        ),
        (
            format!("session_bytes={session_bytes}"),
            format!("session_bytes={}", session_bytes + 1),
        ),
        (
            format!("locked_manifest_sha256={manifest_hash}"),
            format!("locked_manifest_sha256={}", "00".repeat(32)),
        ),
        (
            format!(
                "stop_observed_raw_bytes={STOP_WINDOW_FIRST_BYTE} session_bytes={session_bytes}"
            ),
            format!(
                "session_bytes={session_bytes} stop_observed_raw_bytes={STOP_WINDOW_FIRST_BYTE}"
            ),
        ),
    ];

    for (expected, invalid) in invalid_replacements {
        let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
        let capture_log =
            fs::read_to_string(fixture.sidecar.with_file_name("capture.log")).unwrap();
        let invalid_log = capture_log.replacen(&expected, &invalid, 1);
        assert_ne!(invalid_log, capture_log);
        rebind_capture_log(&fixture, &invalid_log);
        assert!(matches!(
            fixture.reduce(),
            Err(Gate0TraceError::CaptureLogRejected(_))
        ));
    }

    let upper_boundary = write_fixture(&Scenario::default(), &[0, 1], false);
    let capture_log =
        fs::read_to_string(upper_boundary.sidecar.with_file_name("capture.log")).unwrap();
    let upper_boundary_log = capture_log.replacen(
        &format!("stop_observed_raw_bytes={STOP_WINDOW_FIRST_BYTE}"),
        &format!("stop_observed_raw_bytes={STOP_WINDOW_LAST_BYTE}"),
        1,
    );
    rebind_capture_log(&upper_boundary, &upper_boundary_log);
    upper_boundary
        .reduce()
        .expect("the wrapper's inclusive stop-window upper boundary is admitted");
}

#[test]
fn source_artifact_hashes_and_capture_log_fail_closed() {
    let raw_mismatch = write_fixture(&Scenario::default(), &[0, 1], false);
    let mut raw = fs::read(&raw_mismatch.raw_capture).unwrap();
    raw[0] ^= 1;
    fs::write(&raw_mismatch.raw_capture, raw).unwrap();
    assert!(matches!(
        raw_mismatch.reduce(),
        Err(Gate0TraceError::RawCaptureHashMismatch)
    ));

    let binary_mismatch = write_fixture(&Scenario::default(), &[0, 1], false);
    let mut binary = fs::read(&binary_mismatch.capture_binary).unwrap();
    binary[0] ^= 1;
    fs::write(&binary_mismatch.capture_binary, binary).unwrap();
    assert!(matches!(
        binary_mismatch.reduce(),
        Err(Gate0TraceError::ArtifactHashMismatch("capture binary"))
    ));

    let firmware_mismatch = write_fixture(&Scenario::default(), &[0, 1], false);
    let mut firmware = fs::read(&firmware_mismatch.firmware_bundle).unwrap();
    firmware[0] ^= 1;
    fs::write(&firmware_mismatch.firmware_bundle, firmware).unwrap();
    assert!(matches!(
        firmware_mismatch.reduce(),
        Err(Gate0TraceError::ArtifactHashMismatch(
            "device firmware bundle"
        ))
    ));

    let log_mismatch = write_fixture(&Scenario::default(), &[0, 1], false);
    let log_path = log_mismatch.sidecar.with_file_name("capture.log");
    let mut log = fs::read(&log_path).unwrap();
    log[0] ^= 1;
    fs::write(log_path, log).unwrap();
    assert!(matches!(
        log_mismatch.reduce(),
        Err(Gate0TraceError::ArtifactHashMismatch("capture log"))
    ));

    for scenario in [
        Scenario {
            bad_capture_log: true,
            ..Scenario::default()
        },
        Scenario {
            oversized_capture_log_line: true,
            ..Scenario::default()
        },
        Scenario {
            wrong_firmware_log_hash: true,
            ..Scenario::default()
        },
        Scenario {
            unterminated_capture_summary: true,
            ..Scenario::default()
        },
        Scenario {
            mismatched_driver_receipt: true,
            ..Scenario::default()
        },
        Scenario {
            trailing_driver_receipt_token: true,
            ..Scenario::default()
        },
        Scenario {
            summary_not_final: true,
            ..Scenario::default()
        },
    ] {
        let fixture = write_fixture(&scenario, &[0, 1], false);
        assert!(matches!(
            fixture.reduce(),
            Err(Gate0TraceError::CaptureLogRejected(_))
        ));
    }
}

#[test]
fn boot_marker_bus_decode_and_marker_order_are_waveform_derived() {
    for (scenario, expected) in [
        (
            Scenario {
                missing_boot_anchor: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::BootAnchorMissing,
        ),
        (
            Scenario {
                short_boot_marker: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::BootAnchorWidth,
        ),
        (
            Scenario {
                bus_activity_before_boot: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::ActivityBeforeBootAnchor,
        ),
        (
            Scenario {
                ambiguous_bus_transition: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::AmbiguousBusTransition,
        ),
        (
            Scenario {
                malformed_i2c: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::I2cProtocol,
        ),
        (
            Scenario {
                misordered_d9: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::D9MarkerOrder,
        ),
        (
            Scenario {
                marker_bus_transition_overlap: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::MarkerBusTransitionOverlap,
        ),
        (
            Scenario {
                missing_d9: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::D9Missing,
        ),
        (
            Scenario {
                d8_during_d9: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::D8D9Overlap,
        ),
        (
            Scenario {
                short_normal_marker: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::MarkerWidth,
        ),
    ] {
        let sequences: &[u32] = if scenario.jitter_average_hides_outliers {
            &[0, 1, 2]
        } else {
            &[0, 1]
        };
        let fixture = write_fixture(&scenario, sequences, false);
        let evidence = fixture.reduce().unwrap();
        assert!(has_violation(&evidence, expected), "missing {expected:?}");
    }
}

#[test]
fn full_capture_read_rules_reject_semantic_mutations() {
    for (scenario, expected) in [
        (
            Scenario {
                wrong_mux: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::ReadWithoutInt1,
        ),
        (
            Scenario {
                missing_int1: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::ReadWithoutInt1,
        ),
        (
            Scenario {
                ready_before_final: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::ReadyBeforeFinalStatus,
        ),
        (
            Scenario {
                next_edge_crossing: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::NextEdgeCrossing,
        ),
        (
            Scenario {
                four_status_polls: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::PollAverageAtLeastFour,
        ),
        (
            Scenario {
                slow_odr: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::OdrOutOfProvisionalBand,
        ),
        (
            Scenario {
                jitter_average_hides_outliers: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::OdrOutOfProvisionalBand,
        ),
        (
            Scenario {
                wrong_raw_payload: true,
                ..Scenario::default()
            },
            Gate0TraceViolationCode::StaleOrWrongPayload,
        ),
    ] {
        let sequences: &[u32] = if scenario.jitter_average_hides_outliers {
            &[0, 1, 2]
        } else {
            &[0, 1]
        };
        let fixture = write_fixture(&scenario, sequences, false);
        let evidence = fixture.reduce().unwrap();
        assert!(has_violation(&evidence, expected), "missing {expected:?}");
    }

    let wrong_session = write_fixture(&Scenario::default(), &[0, 1], true);
    let evidence = wrong_session.reduce().unwrap();
    assert!(has_violation(
        &evidence,
        Gate0TraceViolationCode::StaleOrWrongPayload
    ));
}

#[test]
fn only_leading_pre_host_snapshots_may_be_unlinked_and_cut_is_final() {
    let leading_drop = write_fixture(&Scenario::default(), &[1], false);
    let evidence = leading_drop.reduce().unwrap();
    assert!(!has_violation(
        &evidence,
        Gate0TraceViolationCode::ExtraAnalyzerSnapshot
    ));

    let trailing_extra = write_fixture(
        &Scenario {
            extra_acquisition_after_final: true,
            ..Scenario::default()
        },
        &[0],
        false,
    );
    let evidence = trailing_extra.reduce().unwrap();
    assert!(has_violation(
        &evidence,
        Gate0TraceViolationCode::AcquisitionAfterFinalCut
    ));
}

#[test]
fn cache_replacement_derives_decimation_and_phase() {
    let scenario = Scenario {
        decimation: true,
        ..Scenario::default()
    };
    let fixture = write_fixture(&scenario, &[0, 1], false);
    let evidence = fixture.reduce().unwrap();

    assert_eq!(evidence.metrics().decimated_reads[0], 1);
    assert_eq!(evidence.metrics().phase_histogram[0].iter().sum::<u64>(), 1);
}

#[test]
fn final_cut_separates_shutdown_unadmitted_edges_from_acquisition_accounting() {
    let fixture = write_fixture(
        &Scenario {
            post_cut_int1: true,
            terminal_idle_samples: Some(TERMINAL_IDLE_MINIMUM_SAMPLES),
            ..Scenario::default()
        },
        &[0, 1],
        false,
    );
    let evidence = fixture.reduce().unwrap();

    assert_eq!(evidence.metrics().int1_edges, [2; SENSOR_COUNT]);
    assert_eq!(
        evidence.metrics().shutdown_unadmitted_int1_edges,
        [1, 0, 0, 0, 0, 0]
    );
    assert_eq!(evidence.metrics().shutdown_transactions, 52);
    assert_eq!(evidence.metrics().shutdown_scl_rising_edges, 1_422);
    assert_eq!(
        evidence.metrics().terminal_idle_samples,
        TERMINAL_IDLE_MINIMUM_SAMPLES
    );
    assert!(evidence.deterministic_quiesce());

    let report = score_gate0a(&fixture.manifest, &fixture.session, &evidence).unwrap();
    assert_eq!(report.criteria().deterministic_quiesce, Gate0Status::Pass);
    let report_json: serde_json::Value =
        serde_json::from_slice(&report.canonical_json().unwrap()).unwrap();
    assert_eq!(report_json["schema"], GATE0A_REPORT_SCHEMA);
    assert_eq!(report_json["criteria"]["deterministic_quiesce"], "pass");
    let trace = &report_json["trace"];
    assert_eq!(trace["schema"], GATE0_CAPTURE_SIDECAR_SCHEMA);
    let metrics = &trace["metrics"];
    assert_eq!(metrics["final_sequence"], 1);
    assert!(metrics["final_d8_rise_sample"].is_u64());
    assert!(metrics["final_d8_fall_sample"].is_u64());
    assert_eq!(
        metrics["shutdown_unadmitted_int1_edges"],
        json!([1, 0, 0, 0, 0, 0])
    );
    assert_eq!(metrics["shutdown_transactions"], 52);
    assert_eq!(metrics["shutdown_scl_rising_edges"], 1_422);
    assert!(metrics["terminal_marker_rise_sample"].is_u64());
    assert!(metrics["terminal_marker_fall_sample"].is_u64());
    assert_eq!(metrics["terminal_marker_width_samples"], 300);
    assert_eq!(
        metrics["terminal_idle_samples"],
        TERMINAL_IDLE_MINIMUM_SAMPLES
    );
    assert_eq!(metrics["host_response_time_ns"], 10_000_000);
    assert_eq!(metrics["deterministic_quiesce_violations"], 0);

    let at_cut = write_fixture(
        &Scenario {
            int1_at_final_cut: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    )
    .reduce()
    .unwrap();
    assert_eq!(at_cut.metrics().int1_edges, [2; SENSOR_COUNT]);
    assert_eq!(
        at_cut.metrics().shutdown_unadmitted_int1_edges,
        [1, 0, 0, 0, 0, 0]
    );
}

#[test]
fn an_unmatched_edge_before_the_final_d8_is_never_forgiven() {
    let fixture = write_fixture(
        &Scenario {
            pre_cut_unmatched_edge: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    );
    let evidence = fixture.reduce().unwrap();

    assert!(has_violation(
        &evidence,
        Gate0TraceViolationCode::UnmatchedPreCutEdge
    ));
    assert!(!evidence.deterministic_quiesce());
}

#[test]
fn every_shutdown_transaction_is_covered_by_each_mutation_class() {
    for transaction_index in 0..SHUTDOWN_TRANSACTION_COUNT {
        for mutation in [
            ShutdownMutation::Omit(transaction_index),
            ShutdownMutation::Duplicate(transaction_index),
            ShutdownMutation::Reorder(transaction_index),
            ShutdownMutation::Nack(transaction_index),
            ShutdownMutation::Corrupt(transaction_index),
            ShutdownMutation::Truncate(transaction_index),
        ] {
            let fixture = write_fixture(
                &Scenario {
                    shutdown_mutation: Some(mutation),
                    ..Scenario::default()
                },
                &[0, 1],
                false,
            );
            let evidence = fixture.reduce().unwrap();
            assert!(
                !evidence.deterministic_quiesce(),
                "transaction {transaction_index} mutation {mutation:?} passed"
            );
            assert!(
                has_violation(
                    &evidence,
                    Gate0TraceViolationCode::ShutdownTransactionMismatch
                ) || has_violation(&evidence, Gate0TraceViolationCode::ShutdownSuffixIncomplete),
                "transaction {transaction_index} mutation {mutation:?} lacked a shutdown witness"
            );
        }
    }
}

#[test]
fn terminal_marker_width_boundaries_and_stagger_are_exact() {
    for (width, accepted) in [(199, false), (200, true), (400, true), (401, false)] {
        let fixture = write_fixture(
            &Scenario {
                terminal_marker_width_ticks: Some(width),
                ..Scenario::default()
            },
            &[0, 1],
            false,
        );
        let evidence = fixture.reduce().unwrap();
        assert_eq!(
            has_violation(&evidence, Gate0TraceViolationCode::TerminalMarkerWidth),
            !accepted,
            "width {width}"
        );
    }

    let staggered = write_fixture(
        &Scenario {
            staggered_terminal_marker: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    )
    .reduce()
    .unwrap();
    assert!(has_violation(
        &staggered,
        Gate0TraceViolationCode::TerminalMarkerShape
    ));
}

#[test]
fn premarker_and_terminal_idle_boundaries_are_exact() {
    for (idle_ticks, accepted) in [(399, false), (400, true)] {
        let evidence = write_fixture(
            &Scenario {
                premarker_idle_ticks: Some(idle_ticks),
                ..Scenario::default()
            },
            &[0, 1],
            false,
        )
        .reduce()
        .unwrap();
        assert_eq!(
            has_violation(
                &evidence,
                Gate0TraceViolationCode::TerminalPremarkerIdleTooShort
            ),
            !accepted,
            "premarker idle {idle_ticks}"
        );
    }

    for (idle_ticks, accepted) in [(399, false), (400, true)] {
        let evidence = write_fixture(
            &Scenario {
                premarker_interrupt: true,
                premarker_idle_ticks: Some(idle_ticks),
                ..Scenario::default()
            },
            &[0, 1],
            false,
        )
        .reduce()
        .unwrap();
        assert_eq!(
            has_violation(
                &evidence,
                Gate0TraceViolationCode::TerminalPremarkerIdleTooShort
            ),
            !accepted,
            "premarker idle after interruption {idle_ticks}"
        );
    }

    for (idle_samples, accepted) in [
        (TERMINAL_IDLE_MINIMUM_SAMPLES - 1, false),
        (TERMINAL_IDLE_MINIMUM_SAMPLES, true),
    ] {
        let evidence = write_fixture(
            &Scenario {
                terminal_idle_samples: Some(idle_samples),
                ..Scenario::default()
            },
            &[0, 1],
            false,
        )
        .reduce()
        .unwrap();
        assert_eq!(evidence.deterministic_quiesce(), accepted);
        assert_eq!(evidence.metrics().terminal_idle_samples, idle_samples);
    }
}

#[test]
fn every_post_marker_logic_change_and_duplicate_marker_is_rejected() {
    for bit in 0..10 {
        let evidence = write_fixture(
            &Scenario {
                post_marker_mask: Some(1 << bit),
                ..Scenario::default()
            },
            &[0, 1],
            false,
        )
        .reduce()
        .unwrap();
        assert!(
            has_violation(&evidence, Gate0TraceViolationCode::PostMarkerActivity),
            "post-marker bit {bit} changed without a witness"
        );
    }

    let duplicate = write_fixture(
        &Scenario {
            duplicate_terminal_marker: true,
            ..Scenario::default()
        },
        &[0, 1],
        false,
    )
    .reduce()
    .unwrap();
    assert!(has_violation(
        &duplicate,
        Gate0TraceViolationCode::PostMarkerActivity
    ));
}

#[test]
fn violation_witnesses_are_bounded_while_total_is_not() {
    let scenario = Scenario {
        many_reserved_transitions: true,
        ..Scenario::default()
    };
    let fixture = write_fixture(&scenario, &[0, 1], false);
    let evidence = fixture.reduce().unwrap();

    assert_eq!(
        evidence.witnesses().violations.len(),
        MAX_GATE0_TRACE_VIOLATION_WITNESSES
    );
    assert!(evidence.witnesses().violation_count > MAX_GATE0_TRACE_VIOLATION_WITNESSES as u64);
    assert!(evidence.witnesses().violation_overflow > 0);
}

#[test]
fn raw_format_rejects_an_incomplete_u16_word() {
    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    let mut raw = fs::read(&fixture.raw_capture).unwrap();
    raw.pop();
    fs::write(&fixture.raw_capture, &raw).unwrap();
    let mut sidecar: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.sidecar).unwrap()).unwrap();
    sidecar["raw_capture_bytes"] = json!(raw.len());
    sidecar["raw_capture_sha256"] = json!(format_sha256(&sha256(&raw)));
    fs::write(&fixture.sidecar, serde_json::to_vec(&sidecar).unwrap()).unwrap();

    assert!(matches!(
        fixture.reduce(),
        Err(Gate0TraceError::OddRawCaptureLength)
    ));
}

#[test]
#[ignore = "full 14.44 GB logical scan; run explicitly in a Crabbox"]
fn canonical_raw_scan_uses_bounded_streaming_state() {
    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    let original_samples = fs::metadata(&fixture.raw_capture).unwrap().len() / 2;
    let canonical_raw_bytes = GATE0_REQUESTED_SAMPLES * 2;

    // Extending creates a sparse canonical-size file without allocating a
    // second in-memory representation. The reducer must still read every byte
    // before it can report the deliberately stale raw hash.
    let raw = OpenOptions::new()
        .write(true)
        .open(&fixture.raw_capture)
        .unwrap();
    raw.set_len(canonical_raw_bytes).unwrap();
    raw.sync_all().unwrap();

    let capture_log = fixture.sidecar.with_file_name("capture.log");
    let old_log = fs::read_to_string(&capture_log).unwrap();
    let old_sample_field = format!("samples={original_samples} ");
    let canonical_sample_field = format!("samples={GATE0_REQUESTED_SAMPLES} ");
    let new_log = old_log.replacen(&old_sample_field, &canonical_sample_field, 1);
    assert_ne!(new_log, old_log);
    fs::write(&capture_log, new_log.as_bytes()).unwrap();

    let mut sidecar: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.sidecar).unwrap()).unwrap();
    sidecar["raw_capture_bytes"] = json!(canonical_raw_bytes);
    sidecar["requested_samples"] = json!(GATE0_REQUESTED_SAMPLES);
    sidecar["command"][8] = json!(GATE0_REQUESTED_SAMPLES.to_string());
    sidecar["capture_log"]["bytes"] = json!(new_log.len());
    sidecar["capture_log"]["sha256"] = json!(format_sha256(&sha256(new_log.as_bytes())));
    fs::write(&fixture.sidecar, serde_json::to_vec(&sidecar).unwrap()).unwrap();

    assert!(matches!(
        fixture.reduce(),
        Err(Gate0TraceError::RawCaptureHashMismatch)
    ));
    fs::remove_dir_all(fixture.raw_capture.parent().unwrap()).unwrap();
}

#[test]
fn sidecar_artifact_paths_cannot_escape_the_evidence_bundle() {
    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    let mut sidecar: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.sidecar).unwrap()).unwrap();
    sidecar["capture_log"]["path"] = json!("../capture.log");
    fs::write(&fixture.sidecar, serde_json::to_vec(&sidecar).unwrap()).unwrap();

    assert!(matches!(
        fixture.reduce(),
        Err(Gate0TraceError::InvalidArtifactPath(_))
    ));
}

#[cfg(unix)]
#[test]
fn sidecar_artifact_symlinks_cannot_escape_the_evidence_bundle() {
    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    let mut sidecar: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.sidecar).unwrap()).unwrap();
    let bundle = fixture.sidecar.parent().unwrap();
    let original_log = bundle.join(sidecar["capture_log"]["path"].as_str().unwrap());
    let outside_directory = temp_dir();
    let outside_log = outside_directory.join("capture.log");
    fs::copy(original_log, &outside_log).unwrap();
    let linked_log = bundle.join("linked-capture.log");
    std::os::unix::fs::symlink(outside_log, &linked_log).unwrap();
    sidecar["capture_log"]["path"] = json!("linked-capture.log");
    fs::write(&fixture.sidecar, serde_json::to_vec(&sidecar).unwrap()).unwrap();

    assert!(matches!(
        fixture.reduce(),
        Err(Gate0TraceError::InvalidArtifactPath(_))
    ));
}

#[test]
fn fixed_capture_settings_are_not_sidecar_choices() {
    for (field, value) in [
        ("sample_rate_hz", json!(4_000_000)),
        ("word_bytes", json!(1)),
        ("voltage_threshold_low_mv", json!(1_200)),
        ("voltage_threshold_high_mv", json!(3_500)),
        ("capture_finished_unix_ms", json!(1_000)),
    ] {
        let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
        let mut sidecar: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture.sidecar).unwrap()).unwrap();
        sidecar[field] = value;
        fs::write(&fixture.sidecar, serde_json::to_vec(&sidecar).unwrap()).unwrap();
        assert!(matches!(
            fixture.reduce(),
            Err(Gate0TraceError::InvalidSidecarField(_))
        ));
    }

    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    let mut sidecar: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.sidecar).unwrap()).unwrap();
    sidecar["channel_map"]["d8"] = json!(7);
    fs::write(&fixture.sidecar, serde_json::to_vec(&sidecar).unwrap()).unwrap();
    assert!(matches!(
        fixture.reduce(),
        Err(Gate0TraceError::InvalidSidecarField("channel_map"))
    ));

    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    let mut sidecar: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.sidecar).unwrap()).unwrap();
    let command = sidecar["command"].as_array_mut().unwrap();
    let output_format = command
        .iter()
        .position(|arg| arg == "--output-format")
        .unwrap();
    command[output_format] = json!("--format");
    fs::write(&fixture.sidecar, serde_json::to_vec(&sidecar).unwrap()).unwrap();
    assert!(matches!(
        fixture.reduce(),
        Err(Gate0TraceError::InvalidSidecarField("command"))
    ));
}

#[test]
fn raw_fixture_never_uses_reserved_logic_bits_unless_requested() {
    let words = build_waveform(&Scenario::default(), 1).words;
    assert!(words.iter().all(|word| word & !0x03ff == 0));
    assert!(words.iter().any(|word| word & INT1_MASK != 0));
}

#[test]
fn evidence_paths_are_real_files_not_decoder_mocks() {
    let fixture = write_fixture(&Scenario::default(), &[0, 1], false);
    for path in [
        fixture.raw_capture.as_path(),
        fixture.sidecar.as_path(),
        fixture.capture_binary.as_path(),
    ] {
        assert!(path.is_file());
    }
}
