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
    'tests/calibration-engine-contract.rs': '/home/ubuntu/ikb-all-offline-raw-before-v11.rs',
    'tests/neutral-contract.rs': '/home/ubuntu/ikb-independent-neutral-before-v11.rs',
}
originals = {relative: (root / relative).read_bytes() for relative in overlays}
base = ['bazelisk', '--batch', '--host_jvm_args=-Xmx2g', 'test']
flags = ['--nocache_test_results', '--test_output=all', '--jobs=1', '--local_resources=memory=3072']
cases = [
    ('neutral-sensor-time-corrected-before', '//:calibration-engine-contract-test', 'raw_sensor_repeat_in_neutral_returns_source_error_without_panicking'),
    ('offline-unselected-time-corrected-before', '//:calibration-engine-contract-test', 'raw_sensor_repeat_outside_neutral_is_rejected_under_1001ns_clocks'),
    ('offline-aggregate-time-corrected-before', '//:calibration-engine-contract-test', 'raw_aggregate_repeat_with_advancing_sensor_times_is_rejected'),
    ('heldout-negative-drift-corrected-before', '//:calibration-engine-contract-test', 'heldout_999ns_clock_preserves_valid_raw_sensor_advance_at_skew_boundary'),
    ('heldout-positive-drift-corrected-before', '//:calibration-engine-contract-test', 'heldout_1001ns_clock_rejects_raw_sensor_repeat_outside_truth_windows'),
    ('fitting-precheck-source-corrected-before', '//:calibration-engine-contract-test', 'fitting_and_precheck_reject_raw_suffix_fault_before_truth_selection'),
    ('independent-neutral-time-before', '//:neutral-contract-test', 'independently_derived_neutral_rejects_repeated_raw_sample_outside_selected_windows'),
]
receipts = []
try:
    for relative, overlay in overlays.items():
        (root / relative).write_bytes(Path(overlay).read_bytes())
    (proof / 'offline-timestamp-before-source.json').write_text(json.dumps({
        'baseline_manifest_sha256': hashlib.sha256(manifest).hexdigest(),
        'all_original_files_match_frozen_baseline': True,
        'production_changes': False,
        'test_overlays': {relative: hashlib.sha256((root / relative).read_bytes()).hexdigest() for relative in sorted(originals)},
    }, indent=2) + '\n')
    for lane, target, test_filter in cases:
        completed = subprocess.run(base + [target] + flags + ['--test_arg=' + test_filter], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        (proof / (lane + '.log')).write_text(completed.stdout)
        print(completed.stdout, flush=True)
        failures = re.findall(r'^test (\S+) \.\.\. FAILED$', completed.stdout, flags=re.MULTILINE)
        observed = completed.returncode != 0 and 'test result: FAILED.' in completed.stdout and any(name.endswith(test_filter) for name in failures)
        receipt = {'lane': lane, 'exit_code': completed.returncode, 'expected_failing_case': test_filter, 'observed_expected_failure': observed, 'failures': failures}
        receipts.append(receipt)
        print(json.dumps(receipt), flush=True)
finally:
    for relative, original in originals.items():
        (root / relative).write_bytes(original)
    for line in manifest.decode().splitlines():
        expected, relative = line.split('  ', 1)
        assert hashlib.sha256((root / relative).read_bytes()).hexdigest() == expected, relative
    (proof / 'offline-timestamp-before-receipts.json').write_text(json.dumps(receipts, indent=2) + '\n')
    print('RESTORED_EXACT_V11_SOURCE', flush=True)
assert len(receipts) == len(cases) and all(receipt['observed_expected_failure'] for receipt in receipts), receipts
