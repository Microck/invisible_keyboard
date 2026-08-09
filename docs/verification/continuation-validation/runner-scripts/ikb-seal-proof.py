import hashlib
import json
from pathlib import Path
import sys

proof = Path(sys.argv[1])
manifest = proof / 'receipt-hashes.sha256'

def digest(path):
    checksum = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1_048_576), b''):
            checksum.update(chunk)
    return checksum.hexdigest()

files = sorted(path for path in proof.rglob('*') if path.is_file() and path != manifest)
manifest.write_text(''.join(digest(path) + '  ' + path.relative_to(proof).as_posix() + '\n' for path in files))
print(json.dumps({'receipt_files': len(files), 'receipt_manifest_sha256': digest(manifest)}, indent=2))
