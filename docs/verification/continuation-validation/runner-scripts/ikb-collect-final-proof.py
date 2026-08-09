import datetime
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import sys
import unittest
import xml.etree.ElementTree as ET

native = Path(sys.argv[1])
x86 = Path(sys.argv[2])
proof = Path(sys.argv[3])
artifact_archive = Path(sys.argv[4])
proof.mkdir()

def digest(path):
    checksum = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1_048_576), b''):
            checksum.update(chunk)
    return checksum.hexdigest()

source = (native / 'source-before.sha256').read_bytes()
assert source == (native / 'source-after.sha256').read_bytes()
assert source == (x86 / 'source-before.sha256').read_bytes()
assert source == (x86 / 'source-after.sha256').read_bytes()
expected_lanes = {
    'native': ['nonmanual', 'durability-1', 'durability-2', 'durability-3', 'application-smoke', 'browser', 'full-size-reducer', 'native-sanitizers', 'capture-stack-processes'],
    'x86': ['required-output', 'x86-tty-and-footer', 'selected-board-firmware', 'capture-stack-binary'],
}
lanes = []
targets = []
for platform, origin in [('native', native), ('x86', x86)]:
    records = json.loads((origin / 'lanes.json').read_text())
    assert [record['lane'] for record in records] == expected_lanes[platform]
    assert all(record['exit_code'] == 0 for record in records)
    shutil.copytree(origin, proof / platform)
    for record in records:
        record['platform'] = platform
        record['log'] = platform + '/' + record['lane'] + '.log'
        lanes.append(record)
        directory = origin / (record['lane'] + '-junit')
        for xml_path in sorted(directory.rglob('test.xml')):
            xml = ET.parse(xml_path).getroot()
            cases = list(xml.iter('testcase'))
            assert cases, str(xml_path)
            assert not list(xml.iter('failure')), str(xml_path)
            assert not list(xml.iter('error')), str(xml_path)
            assert not list(xml.iter('skipped')), str(xml_path)
            output = '\n'.join(node.text or '' for node in xml.iter('system-out'))
            identity = xml_path.relative_to(directory).parent.as_posix()
            targets.append({
                'platform': platform,
                'lane': record['lane'],
                'target': '//'+ identity.rsplit('/', 1)[0] + ':' + identity.rsplit('/', 1)[1] if '/' in identity else '//:' + identity,
                'junit_cases': len(cases),
                'junit_sha256': digest(xml_path),
                'junit': platform + '/' + xml_path.relative_to(origin).as_posix(),
                'inner_summaries': re.findall(r'test result: [^\n]+|Ran \d+ tests? in [^\n]+', output),
            })

assert len([entry for entry in targets if entry['lane'] == 'nonmanual']) == 40
for lane, count in [('durability-1', 1), ('durability-2', 1), ('durability-3', 1), ('browser', 1), ('full-size-reducer', 1), ('native-sanitizers', 4), ('required-output', 1), ('x86-tty-and-footer', 1), ('capture-stack-processes', 1), ('capture-stack-binary', 1)]:
    assert len([entry for entry in targets if entry['lane'] == lane]) == count, lane

def test_ids(suite):
    for case in suite:
        if isinstance(case, unittest.TestSuite):
            yield from test_ids(case)
        else:
            yield case.id().split('.', 1)[1]

test_source = Path('/home/ubuntu/current-source-snapshot-v13/tools/test-gate0a-capture-stack.py')
specification = importlib.util.spec_from_file_location('capture_contract_coverage', test_source)
capture_contract = importlib.util.module_from_spec(specification)
sys.modules[specification.name] = capture_contract
specification.loader.exec_module(capture_contract)
expected_cases = set(test_ids(unittest.defaultTestLoader.loadTestsFromModule(capture_contract)))
capture_partitions = {}
for platform, lane in [('native', 'capture-stack-processes'), ('x86', 'capture-stack-binary')]:
    log = (proof / platform / (lane + '.log')).read_text()
    matches = re.findall(r'(?m)^(test_\w+) \(__main__\.(\w+)(?:\.\w+)?\) \.\.\. ok$', log)
    cases = [class_name + '.' + method for method, class_name in matches]
    assert len(cases) == len(set(cases)), lane
    assert re.search(r'Ran ' + str(len(cases)) + r' tests? in ', log), lane
    capture_partitions[platform] = sorted(cases)
native_cases = set(capture_partitions['native'])
x86_cases = set(capture_partitions['x86'])
assert not native_cases & x86_cases
assert native_cases | x86_cases == expected_cases
assert len(expected_cases) == 20
capture_coverage = {
    'source_manifest_sha256': digest(native / 'source-before.sha256'),
    'discovered_case_count': len(expected_cases),
    'all_cases_passed_without_overlap': True,
    'partitions': capture_partitions,
    'reason': 'Native Python exercises the unchanged 10 ms capture deadline; the x86 guest executes the pinned x86 binary. No test or application limit changed.',
}
(proof / 'capture-test-coverage.json').write_text(json.dumps(capture_coverage, indent=2) + '\n')

artifacts = proof / 'artifacts'
artifacts.mkdir()
shutil.copyfile(artifact_archive, artifacts / 'validated-x86-firmware-capture-and-source.tar.gz')
native_cli = Path('/home/ubuntu/work/cbx_42955e8aa2c5/invisible_keyboard/bazel-bin/invisible-keyboard')
assert digest(native_cli) == (native / 'artifacts.sha256').read_text().split('  ', 1)[0]
shutil.copyfile(native_cli, artifacts / 'invisible-keyboard-aarch64')
(artifacts / 'invisible-keyboard-aarch64').chmod(0o755)

for original, destination in [
    (native / 'source-before.sha256', 'source-before.sha256'),
    (native / 'source-after.sha256', 'source-after.sha256'),
    (x86 / 'source-before.sha256', 'source-x86-before.sha256'),
    (x86 / 'source-after.sha256', 'source-x86-after.sha256'),
    (Path('/home/ubuntu/continuation-native-environment.json'), 'environment-native.json'),
    (Path('/home/ubuntu/continuation-x86-environment.json'), 'environment-x86.json'),
]:
    shutil.copyfile(original, proof / destination)
shutil.copytree('/home/ubuntu/audit-regression-proof', proof / 'regressions')
for platform, directory in [
    ('native', Path('/home/ubuntu/native-v13-capture-final-job')),
    ('x86', Path('/home/ubuntu/x86-v13-capture-job')),
]:
    completion = json.loads((directory / 'completed.json').read_text())
    assert completion['exit_code'] == 0, platform
    shutil.copytree(directory, proof / 'execution-receipts' / platform)
scripts = proof / 'runner-scripts'
scripts.mkdir()
for name in [
    'ikb-native-proof.py',
    'ikb-x86-proof.py',
    'ikb-after-smoke.py',
    'ikb-study-boundary-smoke.py',
    'ikb-consumer-study-before.py',
    'ikb-consumer-study-after.py',
    'ikb-study-smoke-export.rs',
    'ikb-clock-domain-cli-export.rs',
    'ikb-clock-domain-smoke.py',
    'ikb-timestamp-before.py',
    'ikb-offline-timestamp-before.py',
    'ikb-fitting-timestamp-before.py',
    'ikb-timestamp-after.py',
    'ikb-timestamp-format.py',
    'ikb-timestamp-lint.py',
    'ikb-shutdown-dispatch-probe.py',
    'ikb-shutdown-verify.py',
    'ikb-archive-output-proof.py',
    'ikb-proof-job.py',
    'ikb-check-durability-receipts.py',
    'ikb-prepare-native-capture-resume.py',
    'ikb-restore-native-proof-prefix.py',
    'ikb-package-x86-artifacts.py',
    'ikb-collect-final-proof.py',
]:
    shutil.copyfile(Path('/home/ubuntu') / name, scripts / name)

inventory = {
    'schema': 'invisible-keyboard-continuation-validation-v1',
    'collected_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
    'crabbox_id': 'cbx_42955e8aa2c5',
    'provider': 'external Docker; x86-64 capability VM runs under QEMU inside the same ARM64 Crabbox',
    'source_manifest_sha256': digest(proof / 'source-before.sha256'),
    'source_file_count': len(source.splitlines()),
    'source_identical_before_after_and_across_platforms': True,
    'nonmanual_target_count': 40,
    'uncached_test_results': True,
    'lanes': lanes,
    'targets': targets,
    'capture_test_coverage': capture_coverage,
    'artifact_archive': {
        'path': 'artifacts/validated-x86-firmware-capture-and-source.tar.gz',
        'sha256': digest(artifact_archive),
        'bytes': artifact_archive.stat().st_size,
    },
    'evidence_limits': [
        'No physical controller, IMU, analyzer, or wearable was attached or flashed.',
        'Required Linux output uses a real virtual input device in the non-root x86 guest; sensor input remains synthetic.',
        'The full raw-reducer fixture deliberately rejects a stale hash after scanning all 14,440,000,000 bytes.',
        'All three consecutive durability logs and their independent raw strace archives are retained.',
        'Build actions may use pinned caches; test results are uncached.',
        'Regression-before logs describe earlier source states and are not counted as final frozen-source passes.',
    ],
}
(proof / 'inventory.json').write_text(json.dumps(inventory, indent=2) + '\n')
files = sorted(path for path in proof.rglob('*') if path.is_file())
(proof / 'receipt-hashes.sha256').write_text(''.join(digest(path) + '  ' + path.relative_to(proof).as_posix() + '\n' for path in files))
print(json.dumps({'source_manifest_sha256': inventory['source_manifest_sha256'], 'source_files': inventory['source_file_count'], 'lanes': len(lanes), 'target_receipts': len(targets), 'receipt_files': len(files)}, indent=2), flush=True)
