import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('public_tree', Path(__file__).with_name('check_public_tree.py'))
public_tree = importlib.util.module_from_spec(spec)
spec.loader.exec_module(public_tree)


class PublicTreeTests(unittest.TestCase):
    def test_private_paths_and_public_fixtures(self):
        for path in ['docs/evidence/result.json', 'deployment/nanopi/private/token',
                     'config/live/site.json', 'capture.pcap', 'certificate.key',
                     '.env.local', 'deployment/nanopc/20261011-a2.json']:
            self.assertTrue(public_tree.private_path(path), path)
        for path in ['config/simulation.json', 'config/test/legacy-study/scenes.json',
                     'docs/control4-takeover-checklist.md', 'crates/homesensed/src/web_credentials.rs']:
            self.assertFalse(public_tree.private_path(path), path)

    def test_secret_values_are_identified_not_returned(self):
        value = b'ghp_' + b'a' * 36
        findings = public_tree.content_findings('sample', value)
        self.assertEqual(findings, ['GitHub credential'])
        self.assertNotIn(value.decode(), repr(findings))

    def test_network_examples_require_documentation_addresses(self):
        path = 'deployment/network/example.cli.example'
        self.assertEqual(public_tree.content_findings(path, b'2001:db8:1::1'), [])
        self.assertTrue(public_tree.content_findings(path, b'2606:4700::1'))

    def test_scan_reads_index_not_cleaned_worktree(self):
        with tempfile.TemporaryDirectory() as directory:
            subprocess.run(['git', 'init', '-q', directory], check=True)
            path = Path(directory) / 'sample.txt'
            path.write_bytes(b'ghp_' + b'b' * 36)
            subprocess.run(['git', '-C', directory, 'add', 'sample.txt'], check=True)
            path.write_text('clean worktree')
            self.assertEqual(public_tree.check_index(directory), [('sample.txt', 'GitHub credential')])


if __name__ == '__main__':
    unittest.main()
