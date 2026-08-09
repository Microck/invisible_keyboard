import hashlib
import json
from pathlib import Path
import subprocess

root = Path('/home/ubuntu/work/cbx_42955e8aa2c5/invisible_keyboard')
proof = Path('/home/ubuntu/audit-regression-proof')
manifest = Path('/home/ubuntu/current-source-v10.sha256').read_bytes()
assert hashlib.sha256(manifest).hexdigest() == '00ccaf18e69f0efc51bcb8ac1bd0b0fb749595edad5c9cd48cd56eaa22da3e6b'
overlays = {'tests/replay-contract.rs', 'tests/physical-study-contract.rs'}
for line in manifest.decode().splitlines():
    expected, relative = line.split('  ', 1)
    if relative not in overlays:
        assert hashlib.sha256((root / relative).read_bytes()).hexdigest() == expected, relative
(proof / 'consumer-study-before-source.json').write_text(json.dumps({
    'baseline_manifest_sha256': hashlib.sha256(manifest).hexdigest(),
    'production_matches_frozen_baseline': True,
    'test_overlays': {relative: hashlib.sha256((root / relative).read_bytes()).hexdigest() for relative in sorted(overlays)},
}, indent=2) + '\n')
base = ['bazelisk', '--batch', '--host_jvm_args=-Xmx2g', 'test']
flags = ['--nocache_test_results', '--test_output=all', '--jobs=1', '--local_resources=memory=3072']
cases = [
    ('study-truth-boundaries-before', '//:physical-study-contract-test', 'sealed_truth_window_', ['sealed_truth_window_underflow_is_rejected_before_scoring', 'sealed_truth_window_overflow_is_rejected_before_scoring']),
    ('study-exact-boundaries-before', '//:physical-study-contract-test', 'complete_truth_windows_match_exact_timestamp_endpoints', []),
    ('delayed-consumer-before', '//:replay-contract-test', 'live_post_evidence_consumer_delay_revokes_before_candidate_flush', ['live_post_evidence_consumer_delay_revokes_before_candidate_flush']),
]
for name, target, test_filter, failures in cases:
    completed = subprocess.run(base + [target] + flags + ['--test_arg=' + test_filter], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    (proof / (name + '.log')).write_text(completed.stdout)
    print(completed.stdout, flush=True)
    print(json.dumps({'lane': name, 'exit_code': completed.returncode, 'expected_failures': failures}), flush=True)
    if failures:
        assert completed.returncode != 0 and 'test result: FAILED.' in completed.stdout, name
        for case in failures:
            assert case + ' ... FAILED' in completed.stdout, case
    else:
        assert completed.returncode == 0 and test_filter + ' ... ok' in completed.stdout, name
