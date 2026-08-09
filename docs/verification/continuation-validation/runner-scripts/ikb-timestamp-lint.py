import json
from pathlib import Path
import subprocess

root = Path('/home/ubuntu/work/cbx_42955e8aa2c5/invisible_keyboard')
command = [
    'bazelisk', '--batch', '--host_jvm_args=-Xmx2g', 'test',
    '//:clippy-test', '//:rustfmt-test', '--nocache_test_results', '--test_output=all',
    '--jobs=1', '--local_resources=memory=3072',
]
completed = subprocess.run(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
Path('/home/ubuntu/audit-regression-proof/timestamp-settled-lint.log').write_text(completed.stdout)
print(completed.stdout, flush=True)
print(json.dumps({'lane': 'timestamp-settled-lint', 'exit_code': completed.returncode}), flush=True)
raise SystemExit(completed.returncode)
