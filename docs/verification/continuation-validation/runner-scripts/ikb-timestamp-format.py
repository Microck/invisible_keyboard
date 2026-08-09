import json
from pathlib import Path
import subprocess

root = Path('/home/ubuntu/work/cbx_42955e8aa2c5/invisible_keyboard')
cache = Path('/home/ubuntu/.cache/bazel/_bazel_ubuntu/7e2fd77890b488779b410a76e967fea6/external')
formatter = cache / 'rules_rust++rust+rustfmt_nightly-2026-07-16__aarch64-unknown-linux-gnu_tools/bin/rustfmt'
config = cache / 'rules_rust+/rust/settings/.rustfmt.toml'
files = [
    'src/decoder.rs', 'src/runtime.rs', 'src/live.rs', 'src/replay.rs',
    'src/calibration_engine.rs', 'src/calibration_capture.rs', 'src/neutral.rs',
    'src/hardware_calibration.rs', 'tests/runtime-contract.rs', 'tests/replay-contract.rs',
    'tests/calibration-engine-contract.rs', 'tests/calibration-worker-contract.rs',
    'tests/neutral-contract.rs',
]
subprocess.run([str(formatter), '--edition', '2024', '--config-path', str(config), '--config', 'skip_children=true', *files], cwd=root, check=True)
print(json.dumps({'formatted_files': files}), flush=True)
