import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

root, proof = map(Path, sys.argv[1:3])
mode = sys.argv[3]
assert mode in ['native', 'x86']
proof.mkdir(parents=True)
os.chdir(root)
commands = {
    'native': ['bazelisk', '--batch', '--host_jvm_args=-Xmx2g', 'test', '//:clippy-test', '//:rustfmt-test', '//:output-system-contract-test', '--nocache_test_results', '--test_output=all', '--jobs=1', '--local_resources=memory=3072'],
    'x86': ['bazelisk', '--batch', '--host_jvm_args=-Xmx2g', 'test', '//:output-system-required-test', '--nocache_test_results', '--test_output=all', '--jobs=2', '--local_resources=memory=3072'],
}
command = commands[mode]
with (proof / 'verification.log').open('w') as log:
    child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    for line in child.stdout:
        log.write(line)
        print(line, end='', flush=True)
    code = child.wait()
summary = {'mode': mode, 'command': command, 'exit_code': code}
if mode == 'x86' and code == 0:
    executable = root / 'bazel-bin/output-system-required-test'
    summary['test_binary_sha256'] = hashlib.sha256(executable.read_bytes()).hexdigest()
    shutil.copyfile(root / 'bazel-testlogs/output-system-required-test/test.xml', proof / 'test.xml')
(proof / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary), flush=True)
raise SystemExit(code)
