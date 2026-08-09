import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time

binary = Path('/home/tester/invisible_keyboard/bazel-bin/output-system-required-test')
proof = Path('/home/tester/shutdown-dispatch-probe')
proof.mkdir()
assert os.geteuid() != 0
rows = []

def wait_for_path(path, seconds):
    deadline = time.monotonic() + seconds
    while not path.exists():
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.001)
    return True

for mode in ['external-kill', 'direct-kill']:
    for number, argument in [(signal.SIGINT, '-INT'), (signal.SIGTERM, '-TERM')]:
        for cycle in range(10):
            directory = proof / f'{mode}-{argument[1:]}-{cycle}'
            directory.mkdir()
            environment = dict(os.environ)
            environment['INVISIBLE_KEYBOARD_PLANNED_SHUTDOWN_CHILD'] = '1'
            environment['INVISIBLE_KEYBOARD_PLANNED_SHUTDOWN_DIRECTORY'] = str(directory)
            with (directory / 'child.log').open('w') as output:
                child = subprocess.Popen([str(binary), '--exact', 'planned_signals_are_application_observed_neutral_within_250ms', '--nocapture'], env=environment, stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT)
                try:
                    assert wait_for_path(directory / 'held-key-observed', 5), str(directory)
                    started = time.monotonic_ns()
                    if mode == 'external-kill':
                        subprocess.run(['kill', argument, str(child.pid)], check=True)
                    else:
                        os.kill(child.pid, number)
                    dispatched = time.monotonic_ns()
                    neutral = directory / 'application-neutral'
                    receipt_seen = wait_for_path(neutral, 0.250)
                    observed = time.monotonic_ns()
                    native_ns = int(neutral.read_text()) if receipt_seen else None
                    returncode = child.wait(timeout=2)
                    row = {'mode': mode, 'signal': argument, 'cycle': cycle, 'dispatch_us': (dispatched-started)/1000, 'parent_us': (observed-started)/1000, 'native_release_us': native_ns/1000 if native_ns is not None else None, 'receipt_seen': receipt_seen, 'child_exit': returncode, 'within_250ms': receipt_seen and observed-started <= 250_000_000 and native_ns <= 250_000_000 and returncode == 0}
                    rows.append(row)
                    (proof / 'observations.json').write_text(json.dumps(rows, indent=2) + '\n')
                    print(json.dumps(row), flush=True)
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait()

summary = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'effective_uid': os.geteuid(), 'kernel': tuple(os.uname()), 'modes': {}}
for mode in ['external-kill', 'direct-kill']:
    selected = [row for row in rows if row['mode'] == mode]
    summary['modes'][mode] = {'cycles': len(selected), 'deadline_failures': sum(not row['within_250ms'] for row in selected), 'max_dispatch_us': max(row['dispatch_us'] for row in selected), 'max_parent_us': max(row['parent_us'] for row in selected), 'max_native_release_us': max(row['native_release_us'] for row in selected if row['native_release_us'] is not None)}
(proof / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2), flush=True)
