import hashlib
import json
from pathlib import Path
import subprocess

root = Path('/home/ubuntu/work/cbx_42955e8aa2c5/invisible_keyboard')
proof = Path('/home/ubuntu/audit-regression-proof')
manifest = Path('/home/ubuntu/current-source-v11.sha256').read_bytes()
assert hashlib.sha256(manifest).hexdigest() == 'a5675d1a9269009ddbb97eafa44adae5bc2c0c2912b55c251d3a2fdb6ebce78b'
for line in manifest.decode().splitlines():
    expected, relative = line.split('  ', 1)
    assert hashlib.sha256((root / relative).read_bytes()).hexdigest() == expected, relative
path = root / 'tests/calibration-engine-contract.rs'
original = path.read_bytes()
overlay = Path('/home/ubuntu/ikb-all-offline-raw-before-v11.rs').read_bytes()
lane = 'fitting-source-binding-corrected-before'
test_filter = 'fitting_and_precheck_reject_raw_suffix_fault_before_truth_selection'
try:
    path.write_bytes(overlay)
    (proof / (lane + '-source.json')).write_text(json.dumps({
        'baseline_manifest_sha256': hashlib.sha256(manifest).hexdigest(),
        'production_matches_frozen_baseline': True,
        'test_overlay_sha256': hashlib.sha256(overlay).hexdigest(),
    }, indent=2) + '\n')
    completed = subprocess.run([
        'bazelisk', '--batch', '--host_jvm_args=-Xmx2g', 'test',
        '//:calibration-engine-contract-test', '--nocache_test_results', '--test_output=all',
        '--jobs=1', '--local_resources=memory=3072', '--test_arg=' + test_filter,
    ], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    (proof / (lane + '.log')).write_text(completed.stdout)
    print(completed.stdout, flush=True)
    assert completed.returncode != 0 and test_filter + ' ... FAILED' in completed.stdout
finally:
    path.write_bytes(original)
    for line in manifest.decode().splitlines():
        expected, relative = line.split('  ', 1)
        assert hashlib.sha256((root / relative).read_bytes()).hexdigest() == expected, relative
    print('RESTORED_EXACT_V11_SOURCE', flush=True)
