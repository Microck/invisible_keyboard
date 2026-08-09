import hashlib
import json
from pathlib import Path
import sys
import zipfile

root, proof = map(Path, sys.argv[1:3])
source_hash = hashlib.sha256((proof / 'source-before.sha256').read_bytes()).hexdigest()
assert source_hash == '5b6c2109e0a521d3470f895b87f8d4b9667e8a3c72da3fdd510ab234ffa478e6'
lanes = json.loads((proof / 'lanes.json').read_text())
selected = lanes[1:4]
assert [lane['lane'] for lane in selected] == ['durability-1', 'durability-2', 'durability-3']
cli_hash = hashlib.sha256((root / 'bazel-bin/invisible-keyboard').read_bytes()).hexdigest()
test_hash = hashlib.sha256((root / 'bazel-bin/cli-contract-test').read_bytes()).hexdigest()
records = []
for index, lane in enumerate(selected):
    assert lane['exit_code'] == 0 and '--nocache_test_results' in lane['command']
    if index:
        assert selected[index-1]['finished_utc'] <= lane['started_utc']
    with zipfile.ZipFile(proof / (lane['lane'] + '-outputs.zip')) as archive:
        receipt = json.loads(archive.read('live-durability-receipt.json'))
        trace_hash = hashlib.sha256(archive.read('live-durability-strace.log')).hexdigest()
    assert receipt['trace_sha256'] == trace_hash
    assert receipt['cli_sha256'] == cli_hash
    assert receipt['cli_test_sha256'] == test_hash
    assert set(receipt['bundles']) == {'current', 'reset'}
    records.append({'lane': lane['lane'], 'trace_sha256': trace_hash, 'started_utc': lane['started_utc'], 'finished_utc': lane['finished_utc']})
summary = {'source_manifest_sha256': source_hash, 'cli_sha256': cli_hash, 'cli_test_sha256': test_hash, 'three_consecutive_uncached_runs': True, 'runs': records}
(proof / 'durability-binding.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2), flush=True)
