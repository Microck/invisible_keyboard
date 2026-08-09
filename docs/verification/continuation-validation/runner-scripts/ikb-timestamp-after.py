import json
from pathlib import Path
import shutil
import subprocess
import tarfile

root = Path('/home/ubuntu/work/cbx_42955e8aa2c5/invisible_keyboard')
proof = Path('/home/ubuntu/audit-regression-proof')
base = ['bazelisk', '--batch', '--host_jvm_args=-Xmx2g', 'test']
flags = ['--nocache_test_results', '--test_output=all', '--jobs=1', '--local_resources=memory=3072']

def run_lane(name, command):
    completed = subprocess.run(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    (proof / (name + '.log')).write_text(completed.stdout)
    print(completed.stdout, flush=True)
    print(json.dumps({'lane': name, 'exit_code': completed.returncode}), flush=True)
    assert completed.returncode == 0, name

run_lane('timestamp-contracts-after', base + [
    '//:unit-tests', '//:runtime-contract-test', '//:replay-contract-test',
    '//:pipeline-contract-test', '//:calibration-capture-contract-test',
    '//:calibration-worker-contract-test', '//:neutral-contract-test',
    '//:calibration-engine-contract-test',
] + flags)

cli_source = root / 'tests/cli-contract.rs'
canonical_cli = cli_source.read_bytes()
try:
    cli_source.write_bytes(canonical_cli + Path('/home/ubuntu/ikb-clock-domain-cli-export.rs').read_bytes())
    run_lane('clock-domain-cli-fixture-export', base + ['//:cli-contract-test'] + flags + [
        '--test_arg=--exact', '--test_arg=export_clock_domain_cli_fixtures',
        '--test_env=IKB_CLOCK_SMOKE_FIXTURES=/home/ubuntu/clock-domain-cli-fixtures',
    ])
finally:
    cli_source.write_bytes(canonical_cli)

run_lane('clock-domain-direct-cli-after', [
    'python3', '/home/ubuntu/ikb-clock-domain-smoke.py', str(root / 'bazel-bin/invisible-keyboard'),
    str(proof / 'clock-domain-direct-cli-after'), '/home/ubuntu/clock-domain-cli-fixtures',
])
run_lane('timestamp-health-cli-after', base + ['//:cli-contract-test'] + flags + [
    '--test_arg=--exact', '--test_arg=calibrated_live_run_locks_current_and_reset_boots_into_artifact_bound_outputs',
])
run_lane('timestamp-watchdog-cli-after', base + ['//:cli-contract-test'] + flags + [
    '--test_arg=--exact', '--test_arg=calibrated_live_watchdog_revokes_during_continuous_empty_transport_traffic',
])
with tarfile.open(proof / 'clock-domain-cli-fixtures.tar.gz', 'w:gz') as archive:
    archive.add('/home/ubuntu/clock-domain-cli-fixtures', arcname='clock-domain-cli-fixtures')
baseline = proof / 'timestamp-v11-baseline'
baseline.mkdir(exist_ok=True)
for name in [
    'ikb-current-source-v11.tar.gz', 'current-source-v11.sha256',
    'ikb-aggregate-time-regression.rs', 'ikb-frame-clock-domains-before-v11.rs',
    'ikb-all-offline-raw-before-v11.rs', 'ikb-worker-aggregate-before-v11.rs',
    'ikb-monitor-aggregate-before-v11.rs', 'ikb-independent-neutral-before-v11.rs',
]:
    shutil.copyfile(Path('/home/ubuntu') / name, baseline / name)
