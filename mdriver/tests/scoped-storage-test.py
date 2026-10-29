#!/usr/bin/env python3
"""Compile the production patch header with bounded in-memory device fixtures."""
from pathlib import Path
import subprocess,tempfile,re
root=Path(__file__).resolve().parents[1]
patch=(root/'board/mdriver/patches/linux/0093-x86-mboot-pin-bounded-storage-regions.patch').read_text()
protocol=(root.parents[1]/'boot/crates/mdriver-protocol/src/control.rs').read_text()
for name,value in [('OPEN_REGION',27),('REGION_READ',28),('REGION_WRITE',29),('REGION_FLUSH',30),('CLOSE_REGION',31)]:
    assert re.search(rf'pub const MDRIVER_CONTROL_{name}: u16 = {value};',protocol)
    assert f'#define MBOOT_CONTROL_{name} {value}' in patch
body=patch.split('+++ b/arch/x86/kernel/cpu/mboot_scoped_storage.h\n',1)[1]
header=''.join(line[1:]+'\n' for line in body.splitlines() if line.startswith('+') and not line.startswith('+++'))
with tempfile.TemporaryDirectory(prefix='mochios-scoped-storage-') as folder:
    path=Path(folder)
    (path/'mboot_scoped_storage.h').write_text(header)
    subprocess.run(['cc','-std=gnu11','-Wall','-Wextra','-Werror','-I',str(path),str(root/'tests/scoped-storage.c'),'-o',str(path/'probe')],check=True)
    subprocess.run([str(path/'probe')],check=True)
print('Pinned device, bounds, hot removal, stale tokens, flush and short I/O: passed')
