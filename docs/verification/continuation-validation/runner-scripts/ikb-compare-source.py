import datetime
import hashlib
import json
from pathlib import Path
import sys


def manifest(root):
    rows = []
    excluded = {'.git', '.jj', '.crabbox', 'research', '__pycache__'}
    for path in sorted(root.rglob('*')):
        relative = path.relative_to(root)
        if any(part in excluded or part.startswith('bazel-') for part in relative.parts):
            continue
        if relative.as_posix() == 'docs/verification/final-validation-report.md' or relative.parts[:3] in [('docs', 'verification', 'final-validation'), ('docs', 'verification', 'continuation-validation')]:
            continue
        if path.is_file() and not path.is_symlink() and path.suffix != '.pyc':
            checksum = hashlib.sha256()
            with path.open('rb') as stream:
                for chunk in iter(lambda: stream.read(1_048_576), b''):
                    checksum.update(chunk)
            rows.append(checksum.hexdigest() + '  ' + relative.as_posix() + '\n')
    return ''.join(rows)

source = manifest(Path(sys.argv[1]))
tested = manifest(Path(sys.argv[2]))
frozen = Path(sys.argv[3]).read_text()
if source != tested:
    import difflib
    print(''.join(difflib.unified_diff(source.splitlines(True), tested.splitlines(True), fromfile='authoritative', tofile='sandbox')))
assert source == tested, 'authoritative and sandbox sources differ'
assert tested == frozen, 'sandbox differs from frozen manifest'
source_hash = hashlib.sha256(source.encode()).hexdigest()
binding = {
    'checked_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'source_manifest_sha256': source_hash,
    'source_file_count': len(source.splitlines()),
    'authoritative_source_matches_tested_and_frozen': True,
}
Path(sys.argv[4]).write_text(json.dumps(binding, indent=2) + '\n')
print(json.dumps(binding, indent=2))
