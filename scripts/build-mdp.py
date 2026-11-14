#!/usr/bin/env python3
"""Consume boot's verified System Domain, then build only mBoot-owned products."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile


def domain_input(root, destination):
    """Materialize the exact domain from the declared, verified MDP prerequisite."""
    sys.path.insert(0,str(root/'tools/mdp/src'))
    from mdp.cli import digest, require, verify, relative
    require('boot-hardware' in json.loads(os.environ['MDP_REQUIRES']), 'boot-hardware must be a declared prerequisite')
    receipt=json.loads((root/'.mdp/builds.json').read_text())['boot-hardware']
    archive=root/relative(receipt['artifact'])
    require(digest(archive)==receipt['sha256'], 'boot-hardware artifact changed')
    manifest=verify(archive)
    require(manifest['package']==receipt['package'] and manifest['package']['name']=='boot-hardware', 'wrong hardware-domain producer')
    records=[f for f in manifest['files'] if f['area']=='hardware-domains' and f['destination']=='mochios-system']
    require(len(records)==1 and len(manifest['files'])==1, 'expected one hardware System Domain')
    record=records[0]
    destination.parent.mkdir(parents=True,exist_ok=True)
    temporary=destination.with_suffix('.new')
    try:
        with tarfile.open(archive) as source, temporary.open('wb') as output:
            with source.extractfile(record['source']) as stream:shutil.copyfileobj(stream,output)
        require(temporary.stat().st_size==record['size'] and digest(temporary)==record['sha256'], 'hardware System Domain changed during extraction')
        temporary.chmod(record['mode']);temporary.replace(destination)
    finally:temporary.unlink(missing_ok=True)
    return destination


def main():
    """Keep compilation of boot's binaries outside the mBoot producer."""
    root=Path(os.environ['MDP_CONTEXT']).resolve()
    project=Path(__file__).resolve().parents[1]
    domain=domain_input(root,project/'out/mdp/inputs/mochios-system')
    jobs=os.environ.get('MMAKE_JOBS','4')
    subprocess.run(['make','-C',str(project/'mdriver'),'build',f'JOBS={jobs}'],check=True)
    subprocess.run(['make','-C',str(project),'image',
        f'CONFIG={project}/config/intel-hardware.toml',f'MNU_DIR={root}/core',
        f'MOCHIOS_SYSTEM_DIR={root}/boot',f'MOCHIOS_SYSTEM_ELF={domain}',
        f'IMAGE={project}/out/mdp/hardware/mochiOS.img',f'PXE_DIR={project}/out/mdp/pxe',
        f'UEFI_NET_DIR={project}/out/mdp/uefi-net',
        f'MDRIVER_KERNEL={project}/mdriver/output/artifacts/vmlinux',
        f'MDRIVER_INITRAMFS={project}/mdriver/output/artifacts/initramfs.cpio',
        f'MOCHIOS_INITFS={root}/out/artifacts/initfs.img',
        f'MOCHIOS_ROOTFS={root}/out/mmake/image/rootfs.img'],check=True)


if __name__=='__main__':main()
