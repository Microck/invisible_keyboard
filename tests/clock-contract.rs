use invisible_keyboard::clock::{
    ClockError, ClockFit, ClockSample, MAX_CLOCK_DRIFT_PPM, MAX_CLOCK_RESIDUAL_P95_NS,
    MAX_ONLINE_CLOCK_QUALITY_WARMUP_US, MAX_ONLINE_CLOCK_SAMPLES, ONLINE_CLOCK_FIT_WINDOW_US,
    OnlineClockSynchronizer,
};

const BOOT: [u8; 16] = [0x42; 16];

fn clean_samples(drift_ppm: i64) -> Vec<ClockSample> {
    (0..40_u64)
        .map(|index| {
            let device_time_us = index * 25_000;
            let ideal = 7_000_000_000_i128
                + i128::from(device_time_us) * (1_000_000_i128 + i128::from(drift_ppm)) / 1_000;
            let jitter_ns = i128::from((index % 5) as i64 - 2) * 20_000;
            ClockSample {
                device_time_us,
                host_time_ns: u64::try_from(ideal + jitter_ns).expect("fixture time is positive"),
            }
        })
        .collect()
}

#[test]
fn affine_fit_recovers_offset_drift_and_bounded_residuals() {
    let fit = ClockFit::fit(BOOT, &clean_samples(750)).expect("clean fit passes");
    let report = fit.report();

    assert_eq!(report.sample_count, 40);
    assert_eq!(report.boot_id, BOOT);
    assert!(
        (report.drift_ppm - 750.0).abs() < 50.0,
        "estimated drift was {} ppm",
        report.drift_ppm
    );
    assert!(report.residual_p95_ns <= 50_000);
    assert!(report.residual_max_ns <= 50_000);

    let mapped = fit
        .map_device_time_ns(BOOT, 500_000)
        .expect("time inside fit maps");
    assert!(mapped.abs_diff(7_500_375_000) <= 50_000);
}

#[test]
fn too_few_or_too_short_samples_fail_closed() {
    assert_eq!(
        ClockFit::fit(BOOT, &clean_samples(0)[..7]),
        Err(ClockError::TooFewSamples { actual: 7 })
    );

    let short = (0..8_u64)
        .map(|index| ClockSample {
            device_time_us: index * 1_000,
            host_time_ns: 1_000_000_000 + index * 1_000_000,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        ClockFit::fit(BOOT, &short),
        Err(ClockError::SpanTooShort { .. })
    ));
}

#[test]
fn controller_and_host_regressions_are_not_repaired() {
    let mut device_regression = clean_samples(0);
    device_regression[10].device_time_us = device_regression[9].device_time_us;
    assert_eq!(
        ClockFit::fit(BOOT, &device_regression),
        Err(ClockError::DeviceTimeNotIncreasing { index: 10 })
    );

    let mut host_regression = clean_samples(0);
    host_regression[10].host_time_ns = host_regression[9].host_time_ns - 1;
    assert_eq!(
        ClockFit::fit(BOOT, &host_regression),
        Err(ClockError::HostTimeRegression { index: 10 })
    );
}

#[test]
fn implausible_drift_and_bad_residuals_fail() {
    assert!(matches!(
        ClockFit::fit(BOOT, &clean_samples(MAX_CLOCK_DRIFT_PPM as i64 + 500)),
        Err(ClockError::DriftOutOfRange { .. })
    ));

    let mut delayed = clean_samples(0);
    for sample in delayed.iter_mut().step_by(2) {
        sample.host_time_ns += MAX_CLOCK_RESIDUAL_P95_NS * 4;
    }
    assert!(matches!(
        ClockFit::fit(BOOT, &delayed),
        Err(ClockError::ResidualTooLarge { .. })
    ));
}

#[test]
fn mapping_is_bound_to_one_boot_and_bounded_extrapolation() {
    let fit = ClockFit::fit(BOOT, &clean_samples(0)).expect("clean fit passes");

    assert_eq!(
        fit.map_device_time_ns([0x24; 16], 500_000),
        Err(ClockError::BootIdentityMismatch)
    );
    assert!(matches!(
        fit.map_device_time_ns(BOOT, 2_000_000),
        Err(ClockError::ExtrapolationTooLarge { .. })
    ));
}

#[test]
fn online_clock_warms_on_past_only_samples_then_maps_the_current_frame() {
    let mut clock = OnlineClockSynchronizer::new(BOOT);
    let mut first_ready = None;

    for index in 0..40_u64 {
        let device_time_us = index * 5_000;
        let host_time_ns = 3_000_000_000 + device_time_us * 1_000;
        let mapped = clock
            .observe(BOOT, device_time_us, host_time_ns)
            .expect("clean online samples remain valid");
        if let Some(mapped) = mapped
            && first_ready.is_none()
        {
            first_ready = Some((index, mapped));
        }
    }

    let (index, mapped) = first_ready.expect("clock becomes ready");
    assert_eq!(index, 20);
    assert_eq!(mapped, 3_100_000_000);
    assert_eq!(
        clock
            .current_fit()
            .expect("fit is retained")
            .report()
            .sample_count,
        40
    );
}

#[test]
fn online_clock_requires_explicit_reset_for_a_new_boot() {
    let mut clock = OnlineClockSynchronizer::new(BOOT);
    for index in 0..24_u64 {
        clock
            .observe(BOOT, index * 5_000, 4_000_000_000 + index * 5_000_000)
            .expect("first boot warms");
    }
    assert!(clock.current_fit().is_some());
    assert_eq!(
        clock.observe([0x24; 16], 0, 5_000_000_000),
        Err(ClockError::BootIdentityMismatch)
    );

    clock.reset([0x24; 16]);

    assert_eq!(
        clock
            .observe([0x24; 16], 0, 5_000_000_000)
            .expect("new boot starts cleanly"),
        None
    );
    assert!(clock.current_fit().is_none());
}

#[test]
fn online_clock_retries_early_receipt_jitter_without_arming_from_bad_evidence() {
    let mut clock = OnlineClockSynchronizer::new(BOOT);
    let mut first_ready = None;

    for index in 0..100_u64 {
        let device_time_us = index * 5_000;
        // A reader that starts with a draining FIFO can stamp the oldest
        // frames later than their device clock. This linear two-millisecond
        // startup tail makes the first eligible fit exceed the drift gate,
        // then disappears as current receipts take over.
        let startup_delay_ns = if index <= 20 {
            2_000_000_u64.saturating_sub(index * 100_000)
        } else {
            0
        };
        let mapped = clock
            .observe(
                BOOT,
                device_time_us,
                5_000_000_000 + device_time_us * 1_000 + startup_delay_ns,
            )
            .expect("bounded startup receipt jitter remains disarmed, not fatal");
        if mapped.is_some() && first_ready.is_none() {
            first_ready = Some(index);
        }
    }

    assert!(
        first_ready.is_some_and(|index| index > 20),
        "the first rejected fit must not arm, but later clean evidence must recover"
    );
}

#[test]
fn online_clock_bounds_history_and_rejects_sustained_excess_drift() {
    let mut bounded = OnlineClockSynchronizer::new(BOOT);
    for index in 0..1_100_u64 {
        bounded
            .observe(BOOT, index * 5_000, 6_000_000_000 + index * 5_000_000)
            .expect("bounded clean clock remains valid");
    }
    assert_eq!(bounded.sample_count(), MAX_ONLINE_CLOCK_SAMPLES);

    let mut drifting = OnlineClockSynchronizer::new(BOOT);
    let error = (0..120_u64).find_map(|index| {
        let device = index * 5_000;
        let host = 7_000_000_000 + device * 1_006;
        drifting
            .observe(BOOT, device, host)
            .err()
            .map(|error| (device, error))
    });
    assert!(matches!(
        error,
        Some((
            MAX_ONLINE_CLOCK_QUALITY_WARMUP_US,
            ClockError::DriftOutOfRange { .. }
        ))
    ));
}

#[test]
fn online_clock_matches_the_direct_recent_suffix_after_history_wraps() {
    let total_samples = MAX_ONLINE_CLOCK_SAMPLES as u64 + 200;
    let mut clock = OnlineClockSynchronizer::new(BOOT);
    let mut retained = Vec::with_capacity(MAX_ONLINE_CLOCK_SAMPLES);
    let mut expected_accepted_fits = 0_u64;

    for index in 0..total_samples {
        let device_time_us = index * 5_000;
        let sample = ClockSample {
            device_time_us,
            host_time_ns: 7_500_000_000 + device_time_us * 1_000 + (index % 7) * 10_000,
        };
        if retained.len() == MAX_ONLINE_CLOCK_SAMPLES {
            retained.remove(0);
        }
        retained.push(sample);

        let mapped = clock
            .observe(BOOT, sample.device_time_us, sample.host_time_ns)
            .expect("bounded clean samples remain valid across ring wrap");
        let suffix_start = retained.partition_point(|retained_sample| {
            device_time_us - retained_sample.device_time_us > ONLINE_CLOCK_FIT_WINDOW_US
        });

        match ClockFit::fit(BOOT, &retained[suffix_start..]) {
            Ok(expected_fit) => {
                expected_accepted_fits += 1;
                assert_eq!(clock.current_fit(), Some(&expected_fit));
                assert_eq!(
                    mapped,
                    Some(
                        expected_fit
                            .map_device_time_ns(BOOT, device_time_us)
                            .expect("the newest reference sample maps")
                    )
                );
            }
            Err(ClockError::TooFewSamples { .. } | ClockError::SpanTooShort { .. }) => {
                assert_eq!(mapped, None);
                assert!(clock.current_fit().is_none());
            }
            Err(error) => panic!("reference suffix unexpectedly failed: {error}"),
        }
    }

    assert_eq!(clock.sample_count(), MAX_ONLINE_CLOCK_SAMPLES);
    let diagnostics = clock.diagnostics();
    assert_eq!(diagnostics.accepted_fits, expected_accepted_fits);
    assert_eq!(diagnostics.candidate_quality_failures, 0);
    assert_eq!(diagnostics.holdover_frames, 0);
    assert_eq!(diagnostics.maximum_holdover_age_us, 0);
}

#[test]
fn online_clock_fits_only_the_maximal_recent_device_time_suffix() {
    let mut clock = OnlineClockSynchronizer::new(BOOT);
    for index in 0..200_u64 {
        let device_time_us = index * 5_000;
        clock
            .observe(BOOT, device_time_us, 8_000_000_000 + device_time_us * 1_000)
            .expect("clean recent-window samples remain valid");
    }

    let report = clock.current_fit().expect("recent clock fits").report();
    assert_eq!(
        report.last_device_time_us - report.first_device_time_us,
        ONLINE_CLOCK_FIT_WINDOW_US
    );
    assert_eq!(report.sample_count, 51);
    assert_eq!(clock.sample_count(), 200);
}

#[test]
fn online_clock_holds_the_identical_last_good_fit_through_transient_bad_receipts() {
    let mut clock = OnlineClockSynchronizer::new(BOOT);
    for index in 0..60_u64 {
        let device_time_us = index * 5_000;
        clock
            .observe(BOOT, device_time_us, 9_000_000_000 + device_time_us * 1_000)
            .expect("clean clock warms");
    }

    let mut accepted_before_holdover = None;
    for index in 60..80_u64 {
        let device_time_us = index * 5_000;
        let previous = clock.current_fit().copied();
        let mapped = clock
            .observe(
                BOOT,
                device_time_us,
                9_000_000_000 + device_time_us * 1_000 + 8_000_000,
            )
            .expect("bounded bad-receipt burst uses holdover");
        assert!(mapped.is_some());
        if clock.diagnostics().holdover_frames > 0 {
            accepted_before_holdover = previous;
            break;
        }
    }

    let accepted_before_holdover = accepted_before_holdover.expect("bad suffix enters holdover");
    assert_eq!(clock.current_fit().copied(), Some(accepted_before_holdover));
    assert!(clock.diagnostics().candidate_quality_failures > 0);
    assert!(clock.diagnostics().maximum_holdover_age_us <= 250_000);
}

#[test]
fn online_clock_holdover_is_bounded_by_fit_extrapolation() {
    let mut clock = OnlineClockSynchronizer::new(BOOT);
    for index in 0..60_u64 {
        let device_time_us = index * 5_000;
        clock
            .observe(
                BOOT,
                device_time_us,
                10_000_000_000 + device_time_us * 1_000,
            )
            .expect("clean clock warms");
    }

    let accepted = clock.current_fit().copied().expect("clean fit exists");
    let accepted_end = accepted.report().last_device_time_us;
    let at_limit = accepted_end + 250_000;
    let mapped = clock
        .observe(BOOT, at_limit, 10_000_000_000 + at_limit * 1_006)
        .expect("holdover includes the exact extrapolation limit");
    assert!(mapped.is_some());
    assert_eq!(clock.current_fit().copied(), Some(accepted));

    let beyond_limit = at_limit + 1;
    assert!(matches!(
        clock.observe(BOOT, beyond_limit, 10_000_000_000 + beyond_limit * 1_006,),
        Err(ClockError::ExtrapolationTooLarge { actual_us: 250_001 })
    ));
}
