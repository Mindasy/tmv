#!/usr/bin/env python3
"""Release tag/version contract; no network or edits to the real checkout."""
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'tools/release/set-version.py'


class VersionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='tmv-version-')
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        shutil.copytree(ROOT / 'crates', self.root / 'crates')
        for name in ['Cargo.toml', 'Cargo.lock', 'tmv.spec', 'README.md', 'tmv.1',
                     'RELEASE-NOTES.md', 'tools/release/README.md']:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, path)

    def apply(self, tag):
        return subprocess.run([sys.executable, str(SCRIPT), '--root', str(self.root), tag],
                              text=True, capture_output=True)

    def check_tag(self, tag, expected):
        before = (self.root / 'Cargo.lock').read_text()
        result = self.apply(tag)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), expected)
        after = (self.root / 'Cargo.lock').read_text()
        # Dependencies and their checksums must remain byte-for-byte unchanged.
        for old, new in zip(before.split('[[package]]'), after.split('[[package]]')):
            if not re.search(r'^name = "tmv-', old, re.MULTILINE):
                self.assertEqual(old, new)
            else:
                self.assertIn('version = "' + expected + '"', new)
        metadata = subprocess.run(['cargo', 'metadata', '--no-deps', '--locked', '--offline',
                                   '--format-version=1'], cwd=self.root, text=True,
                                  capture_output=True)
        self.assertEqual(metadata.returncode, 0, metadata.stderr)
        import json
        self.assertEqual({p['version'] for p in json.loads(metadata.stdout)['packages']}, {expected})
        self.assertEqual(after, (self.root / 'Cargo.lock').read_text())
        spec = (self.root / 'tmv.spec').read_text()
        self.assertIn('Version:        ' + expected, spec)
        self.assertIn('%global release_tag ' + tag, spec)
        self.assertIn('/releases/download/%{release_tag}/', spec)
        self.assertIn('tmv ' + expected, (self.root / 'tmv.1').read_text())
        self.assertIn('Release tag: ' + tag, (self.root / 'RELEASE-NOTES.md').read_text())
        state = {str(p): p.read_bytes() for p in self.root.rglob('*') if p.is_file()}
        self.assertEqual(self.apply(tag).returncode, 0)
        self.assertEqual(state, {str(p): p.read_bytes() for p in self.root.rglob('*') if p.is_file()})

    def test_plain_tag(self):
        self.check_tag('v0.1.3', '0.1.3')

    def test_dated_tag(self):
        self.check_tag('v1.2.3-20261125', '1.2.3')

    def test_named_suffix(self):
        self.check_tag('v2.10.12-rc.1', '2.10.12')

    def test_invalid_tags_leave_checkout_unchanged(self):
        before = {str(p): p.read_bytes() for p in self.root.rglob('*') if p.is_file()}
        for tag in ['0.1.3', 'v1.2', 'v01.2.3', 'v1.2.3-', 'v1.2.3+meta', 'v1.2.3-x/y']:
            with self.subTest(tag=tag):
                self.assertNotEqual(self.apply(tag).returncode, 0)
                self.assertEqual(before, {str(p): p.read_bytes() for p in self.root.rglob('*') if p.is_file()})


if __name__ == '__main__':
    unittest.main()
