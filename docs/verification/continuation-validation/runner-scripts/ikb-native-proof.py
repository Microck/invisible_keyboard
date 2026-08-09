import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

root = Path(sys.argv[1]).resolve()
proof = Path(sys.argv[2]).resolve()
assert sys.argv[4:] in ([], ['--resume']), 'only --resume may follow the snapshot path'
resuming = bool(sys.argv[4:])
proof.mkdir(parents=True, exist_ok=resuming)
os.environ['PATH'] = '/home/ubuntu/browser-tools/node_modules/node/bin:/home/ubuntu/browser-tools/node_modules/.bin:' + os.environ['PATH']
os.environ['AGENT_BROWSER_BIN'] = '/home/ubuntu/browser-tools/node_modules/.bin/agent-browser'
os.environ['AGENT_BROWSER_EXECUTABLE_PATH'] = '/home/ubuntu/.cache/ms-playwright/chromium-1248/chrome-linux-arm64/chrome'
os.chdir(root)

def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1_048_576), b''):
            value.update(chunk)
    return value.hexdigest()

def source_manifest(source_root=root):
    rows = []
    excluded = {'.git', '.jj', '.crabbox', 'research', '__pycache__'}
    for path in sorted(source_root.rglob('*')):
        relative = path.relative_to(source_root)
        if any(part in excluded or part.startswith('bazel-') for part in relative.parts):
            continue
        if relative.as_posix() == 'docs/verification/final-validation-report.md' or relative.parts[:3] in [('docs', 'verification', 'final-validation'), ('docs', 'verification', 'continuation-validation')]:
            continue
        if path.is_file() and not path.is_symlink() and path.suffix != '.pyc':
            rows.append(digest(path) + '  ' + relative.as_posix() + '\n')
    return ''.join(rows)

before = source_manifest()
if resuming:
    assert before == (proof / 'source-before.sha256').read_text(), 'source changed since the interrupted run'
    lanes = json.loads((proof / 'lanes.json').read_text())
else:
    (proof / 'source-before.sha256').write_text(before)
    lanes = []
assert before == source_manifest(Path(sys.argv[3]).resolve()), 'sandbox differs from authoritative source archive'
print('FROZEN_SOURCE_MANIFEST_SHA256=' + digest(proof / 'source-before.sha256'), flush=True)
lane_index = 0
base = ['bazelisk', '--batch', '--host_jvm_args=-Xmx2g']
flags = ['--nocache_test_results', '--test_output=all', '--jobs=1', '--local_resources=memory=3072']

def run(name, command):
    global lane_index
    if lane_index < len(lanes):
        previous = lanes[lane_index]
        assert previous['lane'] == name and previous['command'] == command and previous['exit_code'] == 0, 'completed lanes must be the unchanged successful command prefix'
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
    row = {'lane': name, 'command': command, 'started_utc': start, 'finished_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'exit_code': code}
    lanes.append(row)
    lane_index += 1
    if 'test' in command:
        receipts = proof / (name + '-junit')
        receipts.mkdir(exist_ok=True)
        tests_root = root / 'bazel-testlogs'
        for target in (argument for argument in command if argument.startswith('//')):
            xmls = tests_root.rglob('test.xml') if target == '//...' else [tests_root / target[2:].replace(':', '/').lstrip('/') / 'test.xml']
            for xml in xmls:
                if xml.is_file() and xml.stat().st_mtime_ns >= started_ns:
                    destination = receipts / xml.relative_to(tests_root)
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(xml, destination)
    if name == 'browser' or name.startswith('durability-'):
        target = 'calibration-browser-receipt-test' if name == 'browser' else 'live-durability-syscall-test'
        outputs = root / ('bazel-testlogs/' + target + '/test.outputs')
        shutil.make_archive(str(proof / (name + '-outputs')), 'zip', outputs)
    (proof / 'lanes.json').write_text(json.dumps(lanes, indent=2) + '\n')
    print('END ' + name + ' exit=' + str(code), flush=True)
    if code:
        raise SystemExit(code)

run('nonmanual', base + ['test', '//...', '--build_tests_only'] + flags)
for index in range(1, 4):
    run('durability-' + str(index), base + ['test', '//:live-durability-syscall-test'] + flags)
run('application-smoke', ['python3', '/home/ubuntu/ikb-after-smoke.py', str(root), str(proof / 'application-smoke')])
run('browser', base + ['test', '//:calibration-browser-receipt-test'] + flags + ['--test_env=PATH', '--test_env=AGENT_BROWSER_BIN', '--test_env=AGENT_BROWSER_EXECUTABLE_PATH'])
run('full-size-reducer', base + ['test', '//:gate0-trace-contract-test'] + flags + ['--test_arg=--ignored', '--test_arg=--exact', '--test_arg=canonical_raw_scan_uses_bounded_streaming_state', '--test_timeout=2400'])
run('native-sanitizers', base + ['test', '//:firmware-frame-conformance-test', '//:firmware-acquisition-conformance-test', '//firmware/zephyr:native-conformance-test', '//firmware/zephyr:native-glue-conformance-test'] + flags + ['--copt=-fsanitize=address,undefined', '--copt=-fno-omit-frame-pointer', '--linkopt=-fsanitize=address,undefined'])
run('capture-stack-processes', base + ['test', '//:gate0a-capture-stack-test'] + flags + ['--test_arg=CaptureContractTests', '--test_arg=FinalizationContractTests', '--test_arg=StaticCaptureStackContractTests', '--test_env=IKB_GATE0A_TEST_FIRMWARE_BUNDLE=/home/ubuntu/gate0a-dslogic-plus-firmware-v3.tar', '--test_timeout=3600'])
after = source_manifest()
(proof / 'source-after.sha256').write_text(after)
assert before == after, 'source changed during frozen verification'
print('SOURCE_MANIFEST_SHA256=' + digest(proof / 'source-before.sha256'), flush=True)
(proof / 'artifacts.sha256').write_text(digest(root / 'bazel-bin/invisible-keyboard') + '  bazel-bin/invisible-keyboard\n')
