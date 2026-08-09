//! Non-authorizing six-face commissioning over the canonical raw wire/session
//! formats. Pose labels come from fixed record order, never a caller's means.

use std::io;

use crate::{
    manifest::{
        AXIS_CHARACTERIZATION_CRITERIA, AXIS_CHARACTERIZATION_METHOD_ID,
        AXIS_CHARACTERIZATION_SCHEMA, AXIS_FACE_ORDER, AxisCharacterizationReport,
        AxisCommissioningCandidate, AxisFaceMean, CANONICAL_AXIS_CONVENTION,
        HandAxisCharacterization, ManifestHand, PositiveYDirection, SensorAxisCharacterization,
        format_id128, format_sha256, is_proper_axis_map, maximum_axis_residual,
    },
    protocol::{ALL_SENSORS_VALID, SENSOR_COUNT, SensorFrame},
    session::{FinalizedSession, SessionReader},
};

pub const SAMPLES_PER_FACE: usize =
    AXIS_CHARACTERIZATION_CRITERIA.minimum_samples_per_face as usize;
pub const FACES_PER_SENSOR: usize = 6;
pub const RECORDS_PER_HAND: usize = SAMPLES_PER_FACE * FACES_PER_SENSOR * SENSOR_COUNT;

/// Identify the real reducer executable and UTC completion time, without
/// asking the operator to supply a build identity or spawning a shell.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn reduction_metadata() -> io::Result<(String, String)> {
    use sha2::{Digest, Sha256};
    use std::{
        fs::File,
        io::Read,
        time::{SystemTime, UNIX_EPOCH},
    };
    let mut executable = File::open(std::env::current_exe()?)?;
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 16384];
    loop {
        let count = executable.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(invalid)?
        .as_secs();
    let time = libc::time_t::try_from(seconds).map_err(invalid)?;
    let mut calendar = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers are valid for the call, and gmtime_r initializes
    // the output only on success. It does not retain either pointer.
    let converted = unsafe { libc::gmtime_r(&time, calendar.as_mut_ptr()) };
    if converted.is_null() {
        return Err(invalid("UTC completion time is out of range"));
    }
    // SAFETY: successful gmtime_r initialized the complete tm above.
    let calendar = unsafe { calendar.assume_init() };
    let utc = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        calendar.tm_year + 1900,
        calendar.tm_mon + 1,
        calendar.tm_mday,
        calendar.tm_hour,
        calendar.tm_min,
        calendar.tm_sec
    );
    Ok((utc, format_sha256(&hash.finalize().into())))
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Linux physical entry point. Only selected stationary blocks are persisted;
/// the reducer below, also used by offline replay, decides whether they pass.
/// No axis report or operational manifest is needed to open these raw TTYs.
#[cfg(target_os = "linux")]
pub fn capture_axes(
    candidate: &AxisCommissioningCandidate,
    device_paths: &[String],
    directory: &std::path::Path,
    session_id: [u8; 16],
    stop: &std::sync::atomic::AtomicBool,
) -> io::Result<()> {
    use crate::{
        controller_source::open_gate0_controller,
        protocol::decode_frame,
        session::{SessionHeader, SessionWriter},
        transport::{TransportCandidate, TransportDecoder},
    };
    use std::{
        fs::{self, File, OpenOptions},
        io::{IsTerminal, Read, Write},
        os::fd::AsRawFd,
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };

    let mut terminal = io::stdin();
    if !terminal.is_terminal() {
        return Err(invalid(
            "characterize-axes needs a terminal for pose acknowledgements",
        ));
    }
    let expected = candidate.expected_controllers();
    if device_paths.len() != expected.len() {
        return Err(invalid(
            "supply exactly one device per candidate hand, in candidate order",
        ));
    }
    let mut devices = device_paths
        .iter()
        .map(|path| open_gate0_controller(path).map_err(invalid))
        .collect::<io::Result<Vec<_>>>()?;
    fs::create_dir(directory)?;
    let partial = directory.join("axis-session.partial");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)?;
    let mut writer = SessionWriter::new(
        file,
        SessionHeader {
            nominal_period_us: 5000,
            manifest_sha256: candidate.sha256(),
            session_id,
        },
    )
    .map_err(invalid)?;
    let mut decoders = vec![TransportDecoder::new(); devices.len()];
    let start = Instant::now();
    let mut pose = 0;
    let pose_count = devices.len() * SENSOR_COUNT * FACES_PER_SENSOR;
    let mut armed_at = None;
    let mut samples = FaceSamples::new();
    let mut last_sample = start;
    let mut boots = vec![None; devices.len()];
    let mut previous_times = vec![None; devices.len()];
    let mut descriptors = vec![libc::pollfd {
        fd: terminal.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    descriptors.extend(devices.iter().map(|device| libc::pollfd {
        fd: device.reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }));
    prompt_pose(&expected, pose)?;
    while pose < pose_count {
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "axis capture cancelled; partial session retained",
            ));
        }
        if start.elapsed() >= Duration::from_secs(1200) {
            return Err(invalid(
                "axis capture exceeded 1200 seconds; restart in a new directory",
            ));
        }
        if !poll_axis_descriptors(&mut descriptors)? {
            continue;
        }
        if descriptors[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(invalid("axis acknowledgement terminal disconnected"));
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            let mut acknowledgement = [0_u8; 16];
            let count = terminal.read(&mut acknowledgement)?;
            if armed_at.is_some() || &acknowledgement[..count] != b"\n" {
                return Err(invalid(
                    "press Enter once per displayed pose, without other text",
                ));
            }
            armed_at = Some(Instant::now());
            last_sample = Instant::now();
        }
        let target_hand = pose / (SENSOR_COUNT * FACES_PER_SENSOR);
        let target_sensor = (pose / FACES_PER_SENSOR) % SENSOR_COUNT;
        let selected =
            armed_at.is_some_and(|time: Instant| time.elapsed() >= Duration::from_secs(1));
        if selected && last_sample.elapsed() > Duration::from_secs(2) {
            return Err(invalid("axis controller stopped delivering samples"));
        }
        let mut pose_complete = false;
        for (index, device) in devices.iter_mut().enumerate() {
            if descriptors[index + 1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                != 0
            {
                return Err(invalid(
                    "axis controller disconnected; partial session retained",
                ));
            }
            if descriptors[index + 1].revents & libc::POLLIN == 0 {
                continue;
            }
            let mut chunk = [0_u8; 4096];
            let count = device.reader.read(&mut chunk)?;
            if count == 0 {
                return Err(invalid("axis controller reached EOF"));
            }
            let host_ns = u64::try_from(start.elapsed().as_nanos()).expect("bounded capture time");
            for transport in decoders[index].push(&chunk[..count]) {
                if !selected || index != target_hand || pose_complete {
                    continue;
                }
                // Preserve a bad selected candidate before returning its error.
                writer
                    .append_transport_candidate(expected[index].0.into(), &transport, host_ns)
                    .map_err(invalid)?;
                let TransportCandidate::Decoded(payload) = transport else {
                    return Err(invalid("transport fault in axis face"));
                };
                let frame = decode_frame(&payload).map_err(invalid)?;
                if frame.hand != expected[index].0
                    || frame.controller_id_sha256 != expected[index].1
                    || boots[index].is_some_and(|boot| boot != frame.boot_id)
                    || previous_times[index].is_some_and(|time| frame.device_time_us <= time)
                {
                    return Err(invalid(
                        "axis controller identity changed or device time regressed",
                    ));
                }
                boots[index] = Some(frame.boot_id);
                previous_times[index] = Some(frame.device_time_us);
                samples.push(&frame, target_sensor)?;
                last_sample = Instant::now();
                if samples.count == SAMPLES_PER_FACE {
                    let completed = std::mem::replace(&mut samples, FaceSamples::new());
                    completed.finish(pose % FACES_PER_SENSOR)?;
                    pose_complete = true;
                }
            }
        }
        if pose_complete {
            pose += 1;
            armed_at = None;
            writer.flush().map_err(invalid)?;
            if pose < pose_count {
                prompt_pose(&expected, pose)?;
            }
        }
    }
    let (file, _) = writer.finish().map_err(invalid)?;
    file.sync_all()?;
    fs::rename(&partial, directory.join("axis-session.iks"))?;
    File::open(directory)?.sync_all()?;
    io::stderr().flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn prompt_pose(expected: &[(crate::layout::Hand, [u8; 32])], pose: usize) -> io::Result<()> {
    use std::io::Write;
    let hand = expected[pose / (SENSOR_COUNT * FACES_PER_SENSOR)].0;
    let sensor = (pose / FACES_PER_SENSOR) % SENSOR_COUNT;
    let face = ["+X", "-X", "+Y", "-Y", "+Z", "-Z"][pose % FACES_PER_SENSOR];
    let mut stderr = io::stderr().lock();
    writeln!(
        stderr,
        "pose={} hand={hand:?} sensor={sensor} face={face} up; hold still, then press Enter (Ctrl-C aborts)",
        pose + 1
    )?;
    stderr.flush()
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn poll_axis_descriptors(descriptors: &mut [libc::pollfd]) -> io::Result<bool> {
    // SAFETY: the slice owns exactly len initialized pollfd entries for the
    // duration of poll. All descriptors remain owned by the caller.
    let status = unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as libc::nfds_t,
            100,
        )
    };
    if status < 0 {
        let error = io::Error::last_os_error();
        // An interrupted poll does not establish readiness. Recheck the stop
        // flag instead of reading from stale revents and potentially blocking.
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error);
    }
    Ok(true)
}

/// A block is one sensor on one fixture face. State resets only at that
/// boundary; gaps between blocks are intentional repositioning, not repaired
/// packet loss within a stationary measurement.
struct FaceSamples {
    count: usize,
    sums: [i64; 3],
    minimum: [i16; 3],
    maximum: [i16; 3],
    first_time_us: u64,
    previous: Option<(u32, u64, u64)>,
}

impl FaceSamples {
    fn new() -> Self {
        Self {
            count: 0,
            sums: [0; 3],
            minimum: [i16::MAX; 3],
            maximum: [i16::MIN; 3],
            first_time_us: 0,
            previous: None,
        }
    }

    fn push(&mut self, frame: &SensorFrame, sensor_index: usize) -> io::Result<()> {
        if frame.valid_sensor_mask != ALL_SENSORS_VALID || frame.flags != 0 {
            return Err(invalid(
                "axis face has invalid sensors or an acquisition/clock fault",
            ));
        }
        let sensor = frame.sensors[sensor_index];
        if sensor
            .acceleration
            .into_iter()
            .chain(sensor.angular_velocity)
            .any(|value| value == i16::MIN || value == i16::MAX)
            || sensor
                .angular_velocity
                .iter()
                .any(|value| i32::from(*value).abs() > 100)
        {
            return Err(invalid(
                "axis face moved or clipped; reposition and restart capture",
            ));
        }
        let sample_time = frame
            .device_time_us
            .checked_add_signed(i64::from(sensor.sample_time_offset_us))
            .ok_or_else(|| invalid("axis sample timestamp is outside the device epoch"))?;
        if let Some((sequence, device_time, previous_sample)) = self.previous {
            if frame.sequence != sequence.wrapping_add(1)
                || frame.device_time_us <= device_time
                || sample_time <= previous_sample
            {
                return Err(invalid(
                    "axis face has a sequence gap or nonadvancing timestamp",
                ));
            }
        } else {
            self.first_time_us = frame.device_time_us;
        }
        for (axis, value) in sensor.acceleration.into_iter().enumerate() {
            self.sums[axis] += i64::from(value);
            self.minimum[axis] = self.minimum[axis].min(value);
            self.maximum[axis] = self.maximum[axis].max(value);
            if i32::from(self.maximum[axis]) - i32::from(self.minimum[axis]) > 820 {
                return Err(invalid("axis face acceleration range exceeds 820 LSB"));
            }
        }
        self.count += 1;
        self.previous = Some((frame.sequence, frame.device_time_us, sample_time));
        Ok(())
    }

    fn finish(self, face_index: usize) -> io::Result<AxisFaceMean> {
        if self.count != SAMPLES_PER_FACE
            || self.previous.expect("nonempty face").1 - self.first_time_us < 2_900_000
        {
            return Err(invalid(
                "axis face needs 600 samples spanning at least 2.9 seconds",
            ));
        }
        Ok(AxisFaceMean {
            face: AXIS_FACE_ORDER[face_index],
            sample_count: SAMPLES_PER_FACE as u32,
            mean_acceleration_lsb: self.sums.map(|sum| (sum / SAMPLES_PER_FACE as i64) as i32),
        })
    }
}

/// Enumerate the small, complete search space before selecting a map. A
/// reflection or ambiguous measurement never receives an arbitrary best fit.
fn select_rotation(means: &[AxisFaceMean; 6]) -> io::Result<([i8; 9], u32)> {
    let mut selected = None;
    for permutation in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        for signs in 0..8 {
            let mut matrix = [0; 9];
            for row in 0..3 {
                matrix[row * 3 + permutation[row]] = if signs & (1 << row) == 0 { 1 } else { -1 };
            }
            if !is_proper_axis_map(matrix) {
                continue;
            }
            let residual = maximum_axis_residual(
                matrix,
                means,
                AXIS_CHARACTERIZATION_CRITERIA.expected_gravity_lsb,
            );
            if residual <= AXIS_CHARACTERIZATION_CRITERIA.maximum_component_residual_lsb {
                if selected.is_some() {
                    return Err(invalid(
                        "axis measurements admit more than one proper rotation",
                    ));
                }
                selected = Some((matrix, residual));
            }
        }
    }
    selected.ok_or_else(|| invalid("axis measurements admit no proper rotation within 820 LSB"))
}

/// Reproduce all numerical evidence from immutable selected raw frames. The
/// metadata identifies this reduction; it does not supply any measured value.
pub fn derive_axes(
    candidate: &AxisCommissioningCandidate,
    session: &FinalizedSession,
    completed_at_utc: &str,
    characterizer_build: &str,
) -> io::Result<AxisCharacterizationReport> {
    if session.header().manifest_sha256 != candidate.sha256()
        || session.header().nominal_period_us != 5000
    {
        return Err(invalid(
            "axis session does not bind this commissioning candidate",
        ));
    }
    let expected = candidate.expected_controllers();
    if session.summary().total_records != (expected.len() * RECORDS_PER_HAND) as u64 {
        return Err(invalid("axis session has missing or extra pose records"));
    }
    let mut reader = SessionReader::new(session);
    let mut hands = Vec::with_capacity(expected.len());
    let mut previous_host = None;
    for ((expected_hand, controller_hash), manifest_hand) in
        expected.iter().zip(&candidate.pending.hands)
    {
        let mut boot = None;
        let mut previous_device = None;
        let mut sensors = Vec::with_capacity(SENSOR_COUNT);
        for sensor_index in 0..SENSOR_COUNT {
            let mut means = Vec::with_capacity(FACES_PER_SENSOR);
            for face_index in 0..FACES_PER_SENSOR {
                let mut samples = FaceSamples::new();
                for _ in 0..SAMPLES_PER_FACE {
                    let record = reader
                        .next_record()
                        .map_err(invalid)?
                        .ok_or_else(|| invalid("axis session ended within a pose"))?;
                    let frame = record.decode().map_err(invalid)?;
                    if frame.hand != *expected_hand
                        || frame.controller_id_sha256 != *controller_hash
                        || boot.is_some_and(|value| value != frame.boot_id)
                    {
                        return Err(invalid("axis source hand, controller, or boot changed"));
                    }
                    if previous_device.is_some_and(|value| frame.device_time_us <= value)
                        || previous_host.is_some_and(|value| record.host_time_ns < value)
                    {
                        return Err(invalid("axis source time regressed"));
                    }
                    boot = Some(frame.boot_id);
                    previous_device = Some(frame.device_time_us);
                    previous_host = Some(record.host_time_ns);
                    samples.push(&frame, sensor_index)?;
                }
                means.push(samples.finish(face_index)?);
            }
            let raw_face_means = means.try_into().expect("six faces");
            let (axis_map, residual) = select_rotation(&raw_face_means)?;
            sensors.push(SensorAxisCharacterization {
                sensor_index: sensor_index as u8,
                role: manifest_hand.sensors[sensor_index].role,
                raw_face_means,
                axis_map,
                observed_maximum_component_residual_lsb: residual,
                passed: true,
            });
        }
        hands.push(HandAxisCharacterization {
            hand: manifest_hand.hand,
            positive_y_direction: match manifest_hand.hand {
                ManifestHand::Left => PositiveYDirection::Ulnar,
                ManifestHand::Right => PositiveYDirection::Radial,
            },
            sensors: sensors.try_into().expect("six sensors"),
        });
    }
    if reader.next_record().map_err(invalid)?.is_some() {
        return Err(invalid("axis session has trailing poses"));
    }
    let report = AxisCharacterizationReport {
        schema: AXIS_CHARACTERIZATION_SCHEMA.to_owned(),
        report_id: format!("axis-{}", format_id128(&session.header().session_id)),
        completed_at_utc: completed_at_utc.to_owned(),
        characterizer_build: characterizer_build.to_owned(),
        method_id: AXIS_CHARACTERIZATION_METHOD_ID.to_owned(),
        coordinate_convention: CANONICAL_AXIS_CONVENTION.to_owned(),
        candidate_manifest_sha256: format_sha256(&candidate.sha256()),
        source_session_sha256: format_sha256(&session.sha256()),
        rig_evidence_sha256: candidate.pending.rig_evidence_sha256.clone(),
        criteria: AXIS_CHARACTERIZATION_CRITERIA,
        hands,
        passed: true,
    };
    report.validate().map_err(invalid)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_search_recovers_every_proper_signed_rotation() {
        let mut recovered = 0;
        // Exhaustive ternary matrices are independent of the production
        // permutation/sign enumeration and contain exactly 24 proper maps.
        for mut code in 0..3_u32.pow(9) {
            let matrix = std::array::from_fn(|_| {
                let component = (code % 3) as i8 - 1;
                code /= 3;
                component
            });
            if !is_proper_axis_map(matrix) {
                continue;
            }
            let means = std::array::from_fn(|face| {
                let row = face / 2;
                let gravity = if face % 2 == 0 { 8192 } else { -8192 };
                AxisFaceMean {
                    face: AXIS_FACE_ORDER[face],
                    sample_count: 600,
                    mean_acceleration_lsb: std::array::from_fn(|column| {
                        i32::from(matrix[row * 3 + column]) * gravity
                    }),
                }
            });
            assert_eq!(select_rotation(&means).unwrap(), (matrix, 0));
            recovered += 1;
        }
        assert_eq!(recovered, 24);
    }
}
