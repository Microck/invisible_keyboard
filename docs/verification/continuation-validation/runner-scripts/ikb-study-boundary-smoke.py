import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

binary = Path(sys.argv[1]).resolve()
proof = Path(sys.argv[2]).resolve()
fixtures = Path(sys.argv[3]).resolve()
proof.mkdir(parents=True)
receipts = []
for name in ['underflow', 'overflow', 'complete-lower', 'complete-upper']:
    accepted = name.startswith('complete-')
    source = fixtures / name
    directory = proof / name
    directory.mkdir()
    row = json.loads((source / 'boundary-row.json').read_text())
    row_id = row['clock']['prompt_id']

    def invoke(command, *paths):
        completed = subprocess.run([str(binary), command, *map(str, paths)], capture_output=True, text=True)
        (directory / (command + '.stdout')).write_text(completed.stdout)
        (directory / (command + '.stderr')).write_text(completed.stderr)
        print(json.dumps({'case': name, 'command': command, 'exit_code': completed.returncode,
                          'stdout': completed.stdout, 'stderr': completed.stderr}), flush=True)
        return completed

    with tempfile.TemporaryDirectory(prefix='ikb-study-boundary-') as temporary:
        work = Path(temporary)
        manifest, plan, evidence = [work / filename for filename in ['manifest.json', 'plan.json', 'evidence.jsonl']]
        report = directory / 'report.json'
        assert invoke('physical-study-plan', source / 'roster.jsonl', manifest, plan).returncode == 0
        assert manifest.read_bytes() == (source / 'expected-manifest.json').read_bytes()
        assert plan.read_bytes() == (source / 'expected-plan.json').read_bytes()
        shutil.copyfile(source / 'evidence-prefix.jsonl', evidence)
        assert invoke('physical-study-seal', source / 'boundary-row.json', evidence).returncode == 0
        evidence_sha256 = hashlib.sha256(evidence.read_bytes()).hexdigest()
        assert evidence_sha256 == (source / 'expected-evidence.sha256').read_text()
        scored = invoke('physical-study-score', evidence, manifest, plan, report)
        receipt = {'case': name, 'interval_admitted': accepted, 'score_exit_code': scored.returncode,
                   'evidence_sha256': evidence_sha256,
                   'manifest_sha256': hashlib.sha256(manifest.read_bytes()).hexdigest(),
                   'plan_sha256': hashlib.sha256(plan.read_bytes()).hexdigest()}
        if accepted:
            assert scored.returncode == 0
            scorecard = json.loads(report.read_text())
            assert scorecard['metrics']['positive_trials'] == 5_001
            assert scorecard['metrics']['matched_events'] == 5_001
            assert scorecard['metrics']['misses'] == 0
            assert scorecard['metrics']['extra_events'] == 0
            if name == 'complete-lower':
                assert scorecard['metrics']['latency_min_us'] == -50_000
            receipt['computed_disposition'] = scorecard['disposition']
            receipt['matched_events'] = scorecard['metrics']['matched_events']
        else:
            assert scored.returncode != 0 and not report.exists()
            assert row_id in scored.stderr and 'truth event' in scored.stderr
        receipts.append(receipt)
(proof / 'summary.json').write_text(json.dumps({
    'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
    'evidence_kind': 'synthetic software-only timestamp-boundary smoke',
    'hardware_evidence': False,
    'cases': receipts,
}, indent=2) + '\n')
print(json.dumps({'study_boundary_cli_smoke': 'passed', 'hardware_evidence': False, 'cases': receipts}), flush=True)
