import hashlib
import json
from pathlib import Path
import subprocess
import sys

binary = Path(sys.argv[1]).resolve()
proof = Path(sys.argv[2]).resolve()
fixtures = Path(sys.argv[3]).resolve()
proof.mkdir(parents=True, exist_ok=True)
spec = json.loads((fixtures / 'clock-domain-cases.json').read_text())
assert spec['hardware_evidence'] is False
before = {str(path.relative_to(fixtures)): hashlib.sha256(path.read_bytes()).hexdigest() for path in fixtures.rglob('*') if path.is_file()}
receipts = []
for case in spec['cases']:
    session = fixtures / case['session']
    assert hashlib.sha256(session.read_bytes()).hexdigest() == case['session_sha256']
    command = [str(binary), 'replay-calibrated', *[str(fixtures / relative) for relative in case['arguments']]]
    completed = subprocess.run(command, text=True, capture_output=True)
    (proof / (case['name'] + '.stdout')).write_text(completed.stdout)
    (proof / (case['name'] + '.stderr')).write_text(completed.stderr)
    print(completed.stdout, end='', flush=True)
    print(completed.stderr, end='', flush=True)
    fields = dict(line.split('=', 1) for line in completed.stdout.splitlines() if '=' in line and not line.startswith('event='))
    events = [line for line in completed.stdout.splitlines() if line.startswith('event=')]
    if case['accept']:
        assert completed.returncode == 0, (case['name'], completed.stderr)
        expected_fields = {
            'evidence': 'replay-tested',
            'records': '202',
            'decoded_frames': '202',
            'left_frames': '101',
            'right_frames': '101',
            'invalid_candidates': '0',
            'runtime_faults': '0',
            'key_decisions': '2',
            'output_events': '4',
            'final_key_state_neutral': 'true',
        }
        for key, value in expected_fields.items():
            assert fields.get(key) == value, (case['name'], key, fields.get(key))
        assert events == case['expected_events'], (events, case['expected_events'])
    else:
        assert completed.returncode != 0, case['name']
        assert 'evidence' not in fields and events == [], completed.stdout
        assert 'record 2' in completed.stderr and 'sensor 0' in completed.stderr and 'strictly advance' in completed.stderr, completed.stderr
    receipt = {
        'scenario': case['name'],
        'slope_ns_per_us': case['slope_ns_per_us'],
        'exit_code': completed.returncode,
        'session_sha256': case['session_sha256'],
        'events': events,
        'hardware_evidence': False,
    }
    receipts.append(receipt)
    print(json.dumps(receipt), flush=True)
after = {str(path.relative_to(fixtures)): hashlib.sha256(path.read_bytes()).hexdigest() for path in fixtures.rglob('*') if path.is_file()}
assert before == after, 'CLI must not rewrite source evidence'
(proof / 'summary.json').write_text(json.dumps({'cases': receipts, 'fixture_files_unchanged': True, 'input_files_sha256': before}, indent=2) + '\n')
