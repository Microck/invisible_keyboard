import datetime
import json
import os
from pathlib import Path
import select
import subprocess
import sys

mode, directory = sys.argv[1], Path(sys.argv[2]).resolve()
command = sys.argv[3:]

if mode == 'worker':
    started = {'pid': os.getpid(), 'command': command, 'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}
    (directory / 'started.json').write_text(json.dumps(started, indent=2) + '\n')
    completed = subprocess.run(command, check=False)
    receipt = dict(started, finished_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(), exit_code=completed.returncode)
    pending = directory / 'completed.pending.json'
    pending.write_text(json.dumps(receipt, indent=2) + '\n')
    pending.replace(directory / 'completed.json')
    raise SystemExit(completed.returncode)

if mode == 'start':
    directory.mkdir()
    with (directory / 'output.log').open('w') as output:
        worker = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), 'worker', str(directory), *command], stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
    print(json.dumps({'worker_pid': worker.pid, 'receipt_directory': str(directory)}), flush=True)
    worker.wait()
elif mode == 'wait':
    if not (directory / 'completed.json').exists():
        started = json.loads((directory / 'started.json').read_text())
        pid = started['pid']
        arguments = (Path('/proc') / str(pid) / 'cmdline').read_bytes().split(b'\0')
        assert str(directory).encode() in arguments and b'worker' in arguments, 'recorded worker no longer owns this process'
        descriptor = os.pidfd_open(pid)
        try:
            select.select([descriptor], [], [])
        finally:
            os.close(descriptor)
else:
    raise SystemExit('expected start, worker, or wait')

receipt = json.loads((directory / 'completed.json').read_text())
print(json.dumps(receipt, indent=2), flush=True)
raise SystemExit(receipt['exit_code'])
