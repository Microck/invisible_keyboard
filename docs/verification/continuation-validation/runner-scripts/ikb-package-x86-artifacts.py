import hashlib
from pathlib import Path
import shutil
import sys
import tarfile

root = Path(sys.argv[1])
proof = Path(sys.argv[2])
bundle = Path(sys.argv[3])
source_archive = Path(sys.argv[4])
bundle.mkdir()

def digest(path):
    checksum = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1_048_576), b''):
            checksum.update(chunk)
    return checksum.hexdigest()

for row in (proof / 'artifacts.sha256').read_text().splitlines():
    expected, name = row.split('  ', 1)
    assert digest(Path(name)) == expected, name
shutil.copyfile(root / 'bazel-bin/invisible-keyboard', bundle / 'invisible-keyboard-x86_64')
(bundle / 'invisible-keyboard-x86_64').chmod(0o755)
firmware = bundle / 'firmware'
firmware.mkdir()
for source in sorted((root / 'bazel-bin/firmware/zephyr').glob('invisible-keyboard-*')):
    if source.is_file():
        shutil.copyfile(source, firmware / source.name)
assert len(list(firmware.glob('*.uf2'))) == 2, 'both left/right UF2 images must be retained'
capture = Path('/home/tester/gate0a-stack-build-v3')
for directory in ['artifacts', 'corresponding-source', 'logs']:
    shutil.copytree(capture / directory, bundle / 'capture-stack' / directory)
shutil.copyfile(capture / 'SHA256SUMS', bundle / 'capture-stack/SHA256SUMS')
shutil.copyfile(proof / 'source-before.sha256', bundle / 'source-manifest.sha256')
shutil.copyfile(source_archive, bundle / 'reference-source.tar.gz')
files = sorted(path for path in bundle.rglob('*') if path.is_file())
(bundle / 'SHA256SUMS').write_text(''.join(digest(path) + '  ' + path.relative_to(bundle).as_posix() + '\n' for path in files))
archive_path = bundle.with_suffix('.tar.gz')
with tarfile.open(archive_path, 'w:gz') as archive:
    archive.add(bundle, arcname=bundle.name)
print('artifact_archive=' + str(archive_path), flush=True)
print('artifact_archive_sha256=' + digest(archive_path), flush=True)
print('artifact_archive_bytes=' + str(archive_path.stat().st_size), flush=True)
