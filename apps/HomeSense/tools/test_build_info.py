import importlib.util
import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(file))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


build = module('build_info', 'generate-build-info.py')
package = module('package_ios', 'package-ios.py')


class BuildInfoTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.app = self.root / 'apps' / 'HomeSense'
        self.app.mkdir(parents=True)
        self.source = self.app / 'Example.swift'
        self.source.write_text('let version = 1\n')
        self.git('init', '-q')
        self.git('add', '.')
        self.git('-c', 'user.name=Build Test', '-c', 'user.email=build@example.invalid', 'commit', '-qm', 'Initial app')

    def git(self, *args):
        env = dict(os.environ, GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)
        return subprocess.check_output(['git', '-C', str(self.root), *args], env=env)

    def test_clean_checkout_uses_real_commit_and_count(self):
        a = build.identity(self.app)
        self.assertEqual(a['HomeSenseGitCommit'], self.git('rev-parse', 'HEAD').decode().strip())
        self.assertEqual(a['CFBundleVersion'], '1')
        self.assertFalse(a['HomeSenseGitDirty'])
        self.assertEqual(a, build.identity(self.app))

    def test_modified_source_changes_fingerprint_and_marks_dirty(self):
        before = build.identity(self.app)
        self.source.write_text('let version = 2\n')
        after = build.identity(self.app)
        self.assertEqual(before['HomeSenseGitCommit'], after['HomeSenseGitCommit'])
        self.assertTrue(after['HomeSenseGitDirty'])
        self.assertNotEqual(before['HomeSenseSourceFingerprint'], after['HomeSenseSourceFingerprint'])

    def test_untracked_app_source_is_included(self):
        before = build.identity(self.app)
        (self.app / 'Added.swift').write_text('let added = true\n')
        after = build.identity(self.app)
        self.assertTrue(after['HomeSenseGitDirty'])
        self.assertNotEqual(before['HomeSenseSourceFingerprint'], after['HomeSenseSourceFingerprint'])

    def test_ignored_credentials_and_unrelated_changes_are_excluded(self):
        (self.root / '.gitignore').write_text('Signing.local.xcconfig\n')
        before = build.identity(self.app)
        (self.app / 'Signing.local.xcconfig').write_text('PRIVATE_SIGNING_DATA\n')
        (self.root / 'unrelated.txt').write_text('other work\n')
        self.assertEqual(before, build.identity(self.app))

    def test_recommit_advances_both_revision_and_numeric_build(self):
        before = build.identity(self.app)
        self.source.write_text('let version = 2\n')
        self.git('add', '.')
        self.git('-c', 'user.name=Build Test', '-c', 'user.email=build@example.invalid', 'commit', '-qm', 'New app')
        after = build.identity(self.app)
        self.assertNotEqual(before['HomeSenseGitCommit'], after['HomeSenseGitCommit'])
        self.assertEqual(after['CFBundleVersion'], '2')
        self.assertFalse(after['HomeSenseGitDirty'])

    def test_generated_plist_retains_product_and_platform_settings(self):
        template = self.root / 'template.plist'
        template.write_bytes(plistlib.dumps({'CFBundleIdentifier': 'example.app', 'CFBundleVersion': '0',
                                             'NSLocalNetworkUsageDescription': 'Local network', 'LSMinimumSystemVersion': '$(MACOSX_DEPLOYMENT_TARGET)'}))
        output = self.root / 'target' / 'generated.plist'
        metadata = build.generate(self.app, template, output)
        info = plistlib.loads(output.read_bytes())
        self.assertEqual(info['CFBundleIdentifier'], 'example.app')
        self.assertEqual(info['LSMinimumSystemVersion'], '$(MACOSX_DEPLOYMENT_TARGET)')
        self.assertEqual(info['CFBundleVersion'], metadata['CFBundleVersion'])

    def test_shallow_history_is_rejected(self):
        original = build.git
        def query(project, *args):
            return b'true\n' if args == ('rev-parse', '--is-shallow-repository') else original(project, *args)
        with patch.object(build, 'git', side_effect=query), self.assertRaisesRegex(ValueError, 'complete Git history'):
            build.identity(self.app)

    def test_source_links_are_rejected(self):
        (self.app / 'Linked.swift').symlink_to(self.source)
        with self.assertRaisesRegex(ValueError, 'symbolic links'):
            build.identity(self.app)

    def test_packaging_preserves_the_compiled_build_identity(self):
        info = build.identity(self.app)
        package.validate_build_metadata(info)
        package.validate_build_metadata(info, info['CFBundleVersion'])
        with self.assertRaisesRegex(ValueError, 'cannot renumber'):
            package.validate_build_metadata(info, '999')

    def test_packaging_rejects_missing_source_metadata(self):
        with self.assertRaisesRegex(ValueError, 'Git source identity'):
            package.validate_build_metadata({'CFBundleVersion': '36'})


if __name__ == '__main__':
    unittest.main()
