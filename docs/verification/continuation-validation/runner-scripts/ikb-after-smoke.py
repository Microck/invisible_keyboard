import hashlib
import json
import os
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

root = Path(sys.argv[1]).resolve()
proof = Path(sys.argv[2]).resolve()
proof.mkdir(parents=True, exist_ok=True)
binary = root / 'bazel-bin/invisible-keyboard'
process = subprocess.Popen([str(binary), 'calibration-demo', '0'], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
stop = threading.Event()
writer = None
slow = None
try:
    for line in process.stdout:
        if line.startswith('url='):
            port = int(line.strip().rsplit(':', 1)[1])
            break
    else:
        raise RuntimeError(process.stderr.read())
    slow = socket.create_connection(('127.0.0.1', port))
    slow.settimeout(2)
    slow.sendall(b'GET / HTTP/1.1\r\nX-Pad: ')
    def trickle():
        while not stop.wait(0.04):
            try:
                slow.sendall(b'x')
            except OSError:
                return
    writer = threading.Thread(target=trickle)
    writer.start()
    started = time.monotonic()
    status = slow.recv(4096).split(b'\r\n')[0]
    elapsed = time.monotonic() - started
    assert status == b'HTTP/1.1 408 Request Timeout', status
    assert elapsed < 0.5, elapsed
    stop.set()
    writer.join()
    with socket.create_connection(('127.0.0.1', port)) as healthy:
        healthy.settimeout(2)
        healthy.sendall(b'GET / HTTP/1.1\r\nHost: localhost\r\n\r\n')
        next_status = healthy.recv(4096).split(b'\r\n')[0]
        assert next_status == b'HTTP/1.1 200 OK', next_status
    print(json.dumps({'scenario': 'progressing-request-after-fix', 'status': status.decode(), 'elapsed_seconds': elapsed, 'next_status': next_status.decode()}), flush=True)
finally:
    stop.set()
    if writer is not None:
        writer.join()
    if slow is not None:
        slow.close()
    process.terminate()
    try:
        process.communicate(timeout=3)
    except subprocess.TimeoutExpired:
        process.kill()
        process.communicate()

# Reuse the real CLI contract's synthetic evidence generator, then invoke the
# application directly on a separate output path and inspect its report.
fixture = proof / 'pilot-fixture'
fixture.mkdir()
environment = os.environ.copy()
environment.update({
    'TEST_TMPDIR': str(fixture),
    'TEST_SRCDIR': str((root / 'bazel-bin/cli-contract-test.runfiles').resolve()),
    'TEST_WORKSPACE': '_main',
    'INVISIBLE_KEYBOARD_RLOCATION': '_main/invisible-keyboard',
    'FIRMWARE_FIXTURE_RLOCATION': '_main/firmware-frame-conformance-test',
})
subprocess.run([str(root / 'bazel-bin/cli-contract-test'), '--exact', 'score_pilot_reduces_bound_raw_evidence_without_authorizing_output', '--nocapture'], env=environment, check=True)
truth_path, = fixture.glob('*.truth.json')
truth = json.loads(truth_path.read_text())
manifest_path = truth_path.with_name(truth_path.name.removesuffix('.truth.json') + '.json')
session_path = manifest_path.with_suffix('.iks')

# That contract ends by removing the final byte. Recover the exact original
# synthetic bytes using its retained SHA-256, not a new footer or relaxed parser.
digest = hashlib.sha256()
with session_path.open('rb') as stream:
    for chunk in iter(lambda: stream.read(1_048_576), b''):
        digest.update(chunk)
for value in range(256):
    candidate = digest.copy()
    candidate.update(bytes([value]))
    if candidate.hexdigest() == truth['session_sha256']:
        with session_path.open('ab') as stream:
            stream.write(bytes([value]))
        break
else:
    raise RuntimeError('contract fixture is not the expected one-byte-truncated original')
rig_path = manifest_path.with_suffix('.rig-evidence.json')
axis_path = manifest_path.with_suffix('.axis-characterization.json')
report_path = proof / 'pilot-direct-report.json'
command = [str(binary), 'score-pilot', str(manifest_path), str(rig_path), str(axis_path), str(session_path), str(truth_path), str(manifest_path.with_suffix('.truth-artifact')), str(manifest_path.with_suffix('.clock-mapping')), str(report_path)]
scored = subprocess.run(command, text=True, capture_output=True)
print(scored.stdout, end='', flush=True)
if scored.returncode:
    raise RuntimeError(scored.stderr)
report = json.loads(report_path.read_text())
assert report['disposition'] == 'stop', report['disposition']
assert report['truth']['session_sha256'] == truth['session_sha256']
assert report['corrected']['contacts'] == 170
assert report['corrected']['correct'] == 0
assert report['corrected']['missing'] == 170
assert report['evidence_level'] == 'replay-tested'
print(json.dumps({'scenario': 'direct-pilot-reduction', 'disposition': report['disposition'], 'contacts': report['corrected']['contacts'], 'correct': report['corrected']['correct'], 'missing': report['corrected']['missing'], 'hardware_evidence': False}), flush=True)

subprocess.run([
    sys.executable,
    str(Path(__file__).with_name('ikb-study-boundary-smoke.py')),
    str(binary),
    str(proof / 'study-boundary-cli'),
    str(Path(__file__).with_name('study-boundary-cli-fixtures')),
], check=True)

subprocess.run([
    sys.executable,
    str(Path(__file__).with_name('ikb-clock-domain-smoke.py')),
    str(binary),
    str(proof / 'clock-domain-cli'),
    str(Path(__file__).with_name('clock-domain-cli-fixtures')),
], check=True)
