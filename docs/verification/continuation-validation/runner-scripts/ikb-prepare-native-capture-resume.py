import json
from pathlib import Path
import shutil

original = Path('/home/ubuntu/continuation-proof-native-v13')
completed = Path('/home/ubuntu/continuation-proof-native-v13-complete')
failed = Path('/home/ubuntu/audit-regression-proof/native-capture-missing-bundle-v13')
lanes = json.loads((original / 'lanes.json').read_text())
assert len(lanes) == 9
assert all(row['exit_code'] == 0 for row in lanes[:8])
assert lanes[-1]['lane'] == 'capture-stack-processes' and lanes[-1]['exit_code'] == 3
failed.mkdir()
for name in ['capture-stack-processes.log', 'lanes.json', 'source-before.sha256']:
    shutil.copyfile(original / name, failed / name)
shutil.copytree(original / 'capture-stack-processes-junit', failed / 'capture-stack-processes-junit')
shutil.copytree('/home/ubuntu/native-v13-capture-job', failed / 'execution-receipt')
shutil.copytree(original, completed, ignore=shutil.ignore_patterns('capture-stack-processes*'))
(completed / 'lanes.json').write_text(json.dumps(lanes[:8], indent=2) + '\n')
print('Preserved the missing-input failure; retained exactly eight successful v13 lanes.')
