import json
from pathlib import Path
import re
import sys

report = Path(sys.argv[1])
repository = Path(sys.argv[2])
proof = Path(sys.argv[3])
links = re.findall(r'\[[^\]]*\]\(([^)]+)\)', report.read_text())
for link in links:
    if link.startswith('continuation-validation/'):
        target = proof / link.removeprefix('continuation-validation/')
    else:
        target = repository / 'docs/verification' / link
    assert target.is_file(), (link, str(target))
receipt = {'all_report_file_links_exist': True, 'link_count': len(links), 'links': links}
(proof / 'report-link-check.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps({'all_report_file_links_exist': True, 'link_count': len(links)}))
