import json
from pathlib import Path
import shutil
import subprocess

root = Path('/home/ubuntu/work/cbx_42955e8aa2c5/invisible_keyboard')
proof = Path('/home/ubuntu/audit-regression-proof')
base = ['bazelisk', '--batch', '--host_jvm_args=-Xmx2g', 'test']
flags = ['--nocache_test_results', '--test_output=all', '--jobs=1', '--local_resources=memory=3072']
lanes = [
    ('consumer-warmup-replay-after', base + ['//:replay-contract-test'] + flags),
    ('study-boundary-fixture-export', base + ['//:physical-study-contract-test'] + flags + ['--test_arg=--exact', '--test_arg=export_complete_study_boundary_cli_fixtures', '--test_env=IKB_STUDY_EXPORT_DIR=/home/ubuntu/study-boundary-cli-fixtures']),
    ('consumer-health-cli-after', base + ['//:cli-contract-test'] + flags + ['--test_arg=--exact', '--test_arg=calibrated_live_run_locks_current_and_reset_boots_into_artifact_bound_outputs']),
    ('consumer-watchdog-cli-after', base + ['//:cli-contract-test'] + flags + ['--test_arg=--exact', '--test_arg=calibrated_live_watchdog_revokes_during_continuous_empty_transport_traffic']),
    ('complete-study-boundary-cli-after', ['python3', '/home/ubuntu/ikb-study-boundary-smoke.py', str(root / 'bazel-bin/invisible-keyboard'), str(proof / 'complete-study-boundary-cli-after'), '/home/ubuntu/study-boundary-cli-fixtures']),
]
try:
    for name, command in lanes:
        completed = subprocess.run(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        (proof / (name + '.log')).write_text(completed.stdout)
        print(completed.stdout, flush=True)
        print(json.dumps({'lane': name, 'exit_code': completed.returncode}), flush=True)
        assert completed.returncode == 0, name
finally:
    shutil.copyfile('/home/ubuntu/ikb-physical-study-canonical.rs', root / 'tests/physical-study-contract.rs')
