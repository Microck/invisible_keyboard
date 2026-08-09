import hashlib
import json
from pathlib import Path
import re
import subprocess

root = Path('/home/ubuntu/work/cbx_42955e8aa2c5/invisible_keyboard')
proof = Path('/home/ubuntu/audit-regression-proof')
manifest = Path('/home/ubuntu/current-source-v11.sha256').read_bytes()
assert hashlib.sha256(manifest).hexdigest() == 'a5675d1a9269009ddbb97eafa44adae5bc2c0c2912b55c251d3a2fdb6ebce78b'
for line in manifest.decode().splitlines():
    expected, relative = line.split('  ', 1)
    assert hashlib.sha256((root / relative).read_bytes()).hexdigest() == expected, relative

overlays = {
    'tests/replay-contract.rs': '/home/ubuntu/ikb-frame-clock-domains-before-v11.rs',
    'tests/calibration-engine-contract.rs': '/home/ubuntu/ikb-all-offline-raw-before-v11.rs',
    'tests/calibration-worker-contract.rs': '/home/ubuntu/ikb-worker-aggregate-before-v11.rs',
    'src/hardware_calibration.rs': '/home/ubuntu/ikb-monitor-aggregate-before-v11.rs',
}
originals = {relative: (root / relative).read_bytes() for relative in [*overlays, 'src/decoder.rs']}
base = ['bazelisk', '--batch', '--host_jvm_args=-Xmx2g', 'test']
flags = ['--nocache_test_results', '--test_output=all', '--jobs=1', '--local_resources=memory=3072']
cases = [
    ('aggregate-time-before', '//:unit-tests', 'repeated_raw_aggregate_time_rejects_even_when_every_sensor_advances'),
    ('replay-positive-drift-before', '//:replay-contract-test', 'replay_positive_drift_rejects_repeated_raw_sensor_before_mapping'),
    ('replay-negative-drift-before', '//:replay-contract-test', 'replay_negative_drift_preserves_raw_sensor_progress_and_mapped_output'),
    ('live-negative-drift-before', '//:replay-contract-test', 'live_negative_drift_consumes_allowed_skew_boundary_and_preserves_mapped_output'),
    ('live-aggregate-time-before', '//:replay-contract-test', 'live_repeated_raw_aggregate_revokes_before_merger_or_evidence_io'),
    ('neutral-sensor-time-before', '//:calibration-engine-contract-test', 'raw_sensor_repeat_in_neutral_returns_source_error_without_panicking'),
    ('offline-unselected-time-before', '//:calibration-engine-contract-test', 'raw_sensor_repeat_outside_neutral_is_rejected_under_1001ns_clocks'),
    ('offline-aggregate-time-before', '//:calibration-engine-contract-test', 'raw_aggregate_repeat_with_advancing_sensor_times_is_rejected'),
    ('worker-aggregate-time-before', '//:calibration-worker-contract-test', 'repeated_aggregate_with_advancing_offsets_faults_worker_and_retains_evidence'),
    ('fitting-heldout-monitor-time-before', '//:unit-tests', 'fitting_and_heldout_monitors_reject_acquisition_faults_and_keep_both_incidents'),
    ('monitor-handoff-time-before', '//:unit-tests', 'monitor_successor_keeps_sequence_and_sensor_time_continuity'),
    ('heldout-negative-drift-before', '//:calibration-engine-contract-test', 'heldout_999ns_clock_preserves_valid_raw_sensor_advance_at_skew_boundary'),
    ('heldout-positive-drift-before', '//:calibration-engine-contract-test', 'heldout_1001ns_clock_rejects_raw_sensor_repeat_outside_truth_windows'),
    ('fitting-precheck-source-before', '//:calibration-engine-contract-test', 'fitting_and_precheck_reject_raw_suffix_fault_before_truth_selection'),
]
receipts = []
try:
    hardware_overlay = Path(overlays['src/hardware_calibration.rs']).read_bytes()
    marker = b'#[cfg(test)]\nmod tests'
    assert marker in hardware_overlay and marker in originals['src/hardware_calibration.rs']
    assert hardware_overlay.split(marker, 1)[0] == originals['src/hardware_calibration.rs'].split(marker, 1)[0]
    for relative, overlay in overlays.items():
        (root / relative).write_bytes(Path(overlay).read_bytes())
    (root / 'src/decoder.rs').write_bytes(originals['src/decoder.rs'] + Path('/home/ubuntu/ikb-aggregate-time-regression.rs').read_bytes())
    (proof / 'timestamp-before-source.json').write_text(json.dumps({
        'baseline_manifest_sha256': hashlib.sha256(manifest).hexdigest(),
        'all_original_files_match_frozen_baseline': True,
        'production_changes': False,
        'test_overlays': {relative: hashlib.sha256((root / relative).read_bytes()).hexdigest() for relative in sorted(originals)},
        'decoder_overlay_kind': 'appended cfg(test) regression module only',
    }, indent=2) + '\n')
    for lane, target, test_filter in cases:
        completed = subprocess.run(base + [target] + flags + ['--test_arg=' + test_filter], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        (proof / (lane + '.log')).write_text(completed.stdout)
        print(completed.stdout, flush=True)
        failures = re.findall(r'^test (\S+) \.\.\. FAILED$', completed.stdout, flags=re.MULTILINE)
        observed = completed.returncode != 0 and 'test result: FAILED.' in completed.stdout and any(name.endswith(test_filter) for name in failures)
        receipt = {'lane': lane, 'exit_code': completed.returncode, 'expected_failing_case': test_filter, 'observed_expected_failure': observed, 'failures': failures}
        receipts.append(receipt)
        if target == '//:calibration-engine-contract-test':
            receipt['overlay_sha256'] = hashlib.sha256((root / 'tests/calibration-engine-contract.rs').read_bytes()).hexdigest()
        print(json.dumps(receipt), flush=True)
finally:
    for relative, original in originals.items():
        (root / relative).write_bytes(original)
    for line in manifest.decode().splitlines():
        expected, relative = line.split('  ', 1)
        assert hashlib.sha256((root / relative).read_bytes()).hexdigest() == expected, relative
    (proof / 'timestamp-before-receipts.json').write_text(json.dumps(receipts, indent=2) + '\n')
    print('RESTORED_EXACT_V11_SOURCE', flush=True)
assert len(receipts) == len(cases) and all(receipt['observed_expected_failure'] for receipt in receipts), receipts
