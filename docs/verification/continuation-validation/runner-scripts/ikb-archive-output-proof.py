import hashlib
import json
from pathlib import Path
import shutil
import zipfile

root = Path('/home/ubuntu/work/cbx_42955e8aa2c5/invisible_keyboard')
proof = Path('/home/ubuntu/audit-regression-proof/v12-native-before-archive-repair')
shutil.copytree('/home/ubuntu/continuation-proof-native-v12', proof)
outputs = root / 'bazel-testlogs/live-durability-syscall-test/test.outputs'
archive = Path(shutil.make_archive(str(proof / 'durability-1-outputs'), 'zip', outputs))
files = sorted(path.relative_to(outputs).as_posix() for path in outputs.rglob('*') if path.is_file())
with zipfile.ZipFile(archive) as stored:
    assert sorted(entry.filename for entry in stored.infolist() if not entry.is_dir()) == files
    for name in files:
        assert stored.read(name) == (outputs / name).read_bytes(), name
summary = {'archive_sha256': hashlib.sha256(archive.read_bytes()).hexdigest(), 'preserved_files': files, 'round_trip_exact': True, 'scope': 'The v12 nonmanual graph and first durability test passed; the old runner then failed by assuming Bazel supplied outputs.zip. These are superseded receipts, not final acceptance.'}
(proof / 'archive-repair-proof.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2), flush=True)
