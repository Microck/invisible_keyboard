import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

root = Path(sys.argv[1]).resolve()
proof = Path(sys.argv[2]).resolve()
assert len(sys.argv) in (4, 5), 'optional fourth argument is the preserved failed proof'
previous_proof = Path(sys.argv[4]).resolve() if len(sys.argv) == 5 else None
proof.mkdir(parents=True)
os.chdir(root)
os.environ['CMAKE_BUILD_PARALLEL_LEVEL'] = '2'

def digest(path):
    checksum = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1_048_576), b''):
            checksum.update(chunk)
    return checksum.hexdigest()

def source_manifest():
    rows = []
    excluded = {'.git', '.jj', '.crabbox', 'research', '__pycache__'}
    for path in sorted(root.rglob('*')):
        relative = path.relative_to(root)
        if any(part in excluded or part.startswith('bazel-') for part in relative.parts):
            continue
        if relative.as_posix() == 'docs/verification/final-validation-report.md' or relative.parts[:3] in [('docs', 'verification', 'final-validation'), ('docs', 'verification', 'continuation-validation')]:
            continue
        if path.is_file() and not path.is_symlink() and path.suffix != '.pyc':
            rows.append(digest(path) + '  ' + relative.as_posix() + '\n')
    return ''.join(rows)

before = source_manifest()
(proof / 'source-before.sha256').write_text(before)
assert before == Path(sys.argv[3]).read_text(), 'x86 source differs from the authoritative frozen source manifest'
print('FROZEN_SOURCE_MANIFEST_SHA256=' + digest(proof / 'source-before.sha256'), flush=True)
lanes = []
if previous_proof is not None:
    assert before == (previous_proof / 'source-before.sha256').read_text(), 'source changed after the failed environment run'
    previous_lanes = json.loads((previous_proof / 'lanes.json').read_text())
    assert [row['lane'] for row in previous_lanes] == ['required-output', 'x86-tty-and-footer', 'selected-board-firmware', 'capture-stack']
    assert previous_lanes[-1]['exit_code'] != 0, 'this resume is only for the recorded capture environment failure'
    lanes = previous_lanes[:3]
    assert all(row['exit_code'] == 0 for row in lanes)
    for row in lanes:
        name = row['lane']
        shutil.copyfile(previous_proof / (name + '.log'), proof / (name + '.log'))
        receipts = previous_proof / (name + '-junit')
        if receipts.is_dir():
            shutil.copytree(receipts, proof / receipts.name)
lane_index = 0
# Reuse the isolated guest's Bazel server; test results remain uncached.
base = ['bazelisk', '--host_jvm_args=-Xmx2g']
flags = ['--nocache_test_results', '--test_output=all', '--jobs=2', '--local_resources=memory=3072']

def run(name, command):
    global lane_index
    if lane_index < len(lanes):
        previous = lanes[lane_index]
        assert previous['lane'] == name and previous['command'] == command and previous['exit_code'] == 0, 'retained lanes must be the unchanged successful command prefix'
        lane_index += 1
        print('RETAINED ' + name + ' exit=0', flush=True)
        return
    start = datetime.datetime.now(datetime.timezone.utc).isoformat()
    started_ns = time.time_ns()
    print('BEGIN ' + name + ' ' + json.dumps(command), flush=True)
    with (proof / (name + '.log')).open('w') as log:
        child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=os.environ)
        for line in child.stdout:
            log.write(line)
            print(line, end='', flush=True)
        code = child.wait()
    lanes.append({'lane': name, 'command': command, 'started_utc': start, 'finished_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'exit_code': code})
    lane_index += 1
    if 'test' in command:
        receipts = proof / (name + '-junit')
        receipts.mkdir(exist_ok=True)
        tests_root = root / 'bazel-testlogs'
        for target in (argument for argument in command if argument.startswith('//')):
            xml = tests_root / target[2:].replace(':', '/').lstrip('/') / 'test.xml'
            if xml.is_file() and xml.stat().st_mtime_ns >= started_ns:
                destination = receipts / xml.relative_to(tests_root)
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(xml, destination)
    (proof / 'lanes.json').write_text(json.dumps(lanes, indent=2) + '\n')
    if code:
        raise SystemExit(code)

run('required-output', base + ['test', '//:output-system-required-test'] + flags)
run('x86-tty-and-footer', base + ['test', '//:cli-contract-test'] + flags + ['--test_arg=gate0_quiesce_cli_contract::'])
run('selected-board-firmware', base + ['build', '//firmware/zephyr:firmware', '--jobs=2', '--local_resources=memory=3072', '--action_env=CMAKE_BUILD_PARALLEL_LEVEL=2', '--action_env=HOME=/home/tester'])
run('capture-stack-binary', base + ['test', '//:gate0a-capture-stack-test'] + flags + ['--test_arg=RealCaptureBinaryContractTests', '--test_env=IKB_GATE0A_TEST_CAPTURE_BINARY=/home/tester/gate0a-stack-build-v3/artifacts/sigrok-cli-gate0a', '--test_env=IKB_GATE0A_TEST_FIRMWARE_BUNDLE=/home/tester/gate0a-stack-build-v3/artifacts/gate0a-dslogic-plus-firmware.tar', '--test_timeout=3600'])
after = source_manifest()
(proof / 'source-after.sha256').write_text(after)
assert before == after, 'source changed during frozen verification'
artifacts = [root / 'bazel-bin/invisible-keyboard'] + list((root / 'bazel-bin/firmware/zephyr').glob('invisible-keyboard-*')) + list(Path('/home/tester/gate0a-stack-build-v3/artifacts').glob('*'))
(proof / 'artifacts.sha256').write_text(''.join(digest(path) + '  ' + str(path) + '\n' for path in artifacts if path.is_file()))
print('SOURCE_MANIFEST_SHA256=' + digest(proof / 'source-before.sha256'), flush=True)
