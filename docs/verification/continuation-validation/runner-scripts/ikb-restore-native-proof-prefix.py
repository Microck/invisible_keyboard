import json
from pathlib import Path
import shutil

proof = Path('/home/ubuntu/continuation-proof-native-v13')
preserved = Path('/home/ubuntu/continuation-proof-native-v13-complete')
failed = Path('/home/ubuntu/audit-regression-proof/native-capture-missing-bundle-v13')
original_lanes = json.loads((proof / 'lanes.json').read_text())
successful_lanes = json.loads((preserved / 'lanes.json').read_text())
assert len(original_lanes) == 9 and original_lanes[-1]['exit_code'] == 3
assert len(successful_lanes) == 8 and original_lanes[:8] == successful_lanes
assert all(row['exit_code'] == 0 for row in successful_lanes)
assert (failed / 'capture-stack-processes.log').read_bytes() == (proof / 'capture-stack-processes.log').read_bytes()
shutil.copytree('/home/ubuntu/native-v13-capture-complete-job', failed / 'resume-command-mismatch')
(proof / 'lanes.json').write_text(json.dumps(successful_lanes, indent=2) + '\n')
print('Restored the unchanged successful prefix at its recorded proof path.')
