"""Real MDP boundary checks for mBoot's externally built System Domain."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/'tools/mdp/src'))
from mdp.cli import digest, lock_project, pack, verify, Error
spec=importlib.util.spec_from_file_location('mboot_mdp',Path(__file__).with_name('build-mdp.py'))
builder=importlib.util.module_from_spec(spec);spec.loader.exec_module(builder)


class DomainInputs(unittest.TestCase):
    def test_measured_domain_is_used_and_changed_artifact_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'.mdp').mkdir()
            (root/'domain').write_bytes(b'measured System Domain')
            project=root/'mdp.toml'
            project.write_text('format=1\n[package]\nname="boot-hardware"\nversion="1"\ntarget="x86_64-mochios"\nabi="mochios-1"\n[[files]]\nsource="domain"\narea="hardware-domains"\ndestination="mochios-system"\nmode=420\n')
            lock_project(project);archive=root/'domain.tar.mdp';pack(project,archive);manifest=verify(archive)
            (root/'.mdp/builds.json').write_text(json.dumps({'boot-hardware':dict(artifact=archive.name,sha256=digest(archive),package=manifest['package'])}))
            destination=root/'output/domain'
            with patch.dict(os.environ,MDP_REQUIRES='["boot-hardware"]'):
                builder.domain_input(root,destination)
                self.assertEqual(destination.read_bytes(),b'measured System Domain')
                archive.write_bytes(archive.read_bytes()+b'changed')
                with self.assertRaisesRegex(Error,'artifact changed'):builder.domain_input(root,destination)
                self.assertEqual(destination.read_bytes(),b'measured System Domain')
            with patch.dict(os.environ,MDP_REQUIRES='[]'):
                with self.assertRaisesRegex(Error,'declared prerequisite'):builder.domain_input(root,destination)


if __name__=='__main__':unittest.main()
