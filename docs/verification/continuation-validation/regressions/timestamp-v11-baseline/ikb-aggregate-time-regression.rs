
#[cfg(test)]
mod aggregate_time_regression {
    use crate::{
        decoder::{FrameIntegrity, FrameIntegrityError, WindowError},
        layout::Hand,
        protocol::{ALL_SENSORS_VALID, SENSOR_COUNT, SensorFrame, SensorReading},
    };

    #[test]
    fn repeated_raw_aggregate_time_rejects_even_when_every_sensor_advances() {
        let mut frame = SensorFrame {
            hand: Hand::Left,
            controller_id_sha256: [0x11; 32],
            boot_id: [0x31; 16],
            flags: 0,
            valid_sensor_mask: ALL_SENSORS_VALID,
            sequence: 20,
            device_time_us: 100_000,
            sensors: [SensorReading {
                sample_time_offset_us: -1_000,
                ..SensorReading::default()
            }; SENSOR_COUNT],
        };
        let mut integrity = FrameIntegrity::default();
        integrity.observe(&frame).expect("first raw frame is healthy");
        frame.sequence += 1;
        for sensor in &mut frame.sensors {
            sensor.sample_time_offset_us += 1;
        }
        assert_eq!(
            integrity.observe(&frame),
            Err(FrameIntegrityError::Structure(WindowError::TimeRegression))
        );
    }
}
