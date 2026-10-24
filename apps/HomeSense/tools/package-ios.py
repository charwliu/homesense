#!/usr/bin/env python3
"""Sign a standalone iOS app, including Xcode's root debug/preview dylibs."""
import argparse
import datetime
import hashlib
import json
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys

MACH_O_MAGICS = {
    b'\xfe\xed\xfa\xce', b'\xce\xfa\xed\xfe',
    b'\xfe\xed\xfa\xcf', b'\xcf\xfa\xed\xfe',
    b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca',
    b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca',
}


def run(*args):
    result = subprocess.run(args, capture_output=True)
    if result.returncode:
        raise RuntimeError((result.stderr or result.stdout).decode(errors='replace').strip())
    return result


def code_files(app):
    files = []
    for path in sorted(app.rglob('*')):
        if path.is_symlink():
            raise ValueError('Standalone iOS package must not contain symlinks')
        if path.is_file():
            with path.open('rb') as stream:
                if stream.read(4) in MACH_O_MAGICS:
                    files.append(path)
    return files


def verify(app, bundle, team):
    info = plistlib.loads((app / 'Info.plist').read_bytes())
    validate_build_metadata(info)
    if info['CFBundleIdentifier'] != bundle:
        raise ValueError('Unexpected application bundle identifier')
    files = code_files(app)
    if app / info['CFBundleExecutable'] not in files:
        raise ValueError('Main application executable is missing')
    # --deep alone misses unsigned root HomeSense.debug.dylib / __preview.dylib.
    for path in files:
        run('codesign', '--verify', '--strict', str(path))
        details = run('codesign', '-d', '--verbose=4', str(path)).stderr.decode()
        if f'TeamIdentifier={team}\n' not in details:
            raise ValueError(f'Unexpected signing team: {path.name}')
    run('codesign', '--verify', '--deep', '--strict', str(app))
    entitlements = plistlib.loads(run('codesign', '-d', '--entitlements', ':-', str(app)).stdout)
    if entitlements.get('application-identifier') != team + '.' + bundle:
        raise ValueError('Unexpected signed application identifier')
    if entitlements.get('com.apple.developer.team-identifier') != team:
        raise ValueError('Unexpected signed entitlement team')
    profile = plistlib.loads(run('security', 'cms', '-D', '-i', str(app / 'embedded.mobileprovision')).stdout)
    if team not in profile['TeamIdentifier']:
        raise ValueError('Provisioning profile team differs')
    expiry = profile['ExpirationDate'].replace(tzinfo=datetime.timezone.utc)
    if expiry <= datetime.datetime.now(datetime.timezone.utc):
        raise ValueError('Provisioning profile has expired')
    permitted = profile['Entitlements']['application-identifier']
    if permitted not in (team + '.*', team + '.' + bundle):
        raise ValueError('Provisioning profile does not permit this application')
    return {
        'status': 'PASS_ALL_MACHO_SIGNATURES',
        'bundle': bundle, 'build': info['CFBundleVersion'], 'team': team,
        'source_commit': info['HomeSenseGitCommit'],
        'source_dirty': info['HomeSenseGitDirty'],
        'source_fingerprint': info['HomeSenseSourceFingerprint'],
        'code_objects': [str(path.relative_to(app)) for path in files],
        'entitlements': entitlements,
        'artifact_tree': [
            {'path': str(path.relative_to(app)), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
            for path in sorted(app.rglob('*')) if path.is_file()
        ],
    }


def validate_build_metadata(info, requested_build=None):
    if not re.fullmatch(r'[1-9][0-9]*', info.get('CFBundleVersion', '')):
        raise ValueError('Missing numeric Git commit-count build')
    if not re.fullmatch(r'[0-9a-f]{40}', info.get('HomeSenseGitCommit', '')):
        raise ValueError('Missing generated Git source identity; rebuild the app')
    if not isinstance(info.get('HomeSenseGitDirty'), bool):
        raise ValueError('Missing source modification marker')
    if not re.fullmatch(r'[0-9a-f]{64}', info.get('HomeSenseSourceFingerprint', '')):
        raise ValueError('Missing generated source fingerprint')
    if requested_build is not None and requested_build != info['CFBundleVersion']:
        raise ValueError('Packaging cannot renumber a compiled app; rebuild from the intended Git commit')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', type=Path, required=True)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--build-number')
    parser.add_argument('--identity')
    parser.add_argument('--profile', type=Path)
    parser.add_argument('--entitlements', type=Path)
    parser.add_argument('--expected-bundle', required=True)
    parser.add_argument('--expected-team', required=True)
    parser.add_argument('--verify-only', action='store_true')
    parser.add_argument('--report', type=Path)
    args = parser.parse_args()
    app = args.app.resolve(strict=True)
    if not args.verify_only:
        if not all([args.output, args.identity, args.profile, args.entitlements]):
            parser.error('Packaging requires output, identity, profile and entitlements')
        validate_build_metadata(plistlib.loads((app / 'Info.plist').read_bytes()), args.build_number)
        if (app / 'PlugIns').exists() or (app / 'Watch').exists():
            raise ValueError('Extensions require separate profiles; this packager is standalone only')
        entitlements = plistlib.loads(args.entitlements.read_bytes())
        if entitlements.get('application-identifier') != args.expected_team + '.' + args.expected_bundle:
            raise ValueError('Requested entitlements do not match the original application identity')
        output = args.output.absolute()
        if output.exists():
            raise ValueError('Output package already exists; preserve or remove it explicitly')
        shutil.copytree(app, output)
        app = output
        info_path = app / 'Info.plist'
        info = plistlib.loads(info_path.read_bytes())
        if info['CFBundleIdentifier'] != args.expected_bundle:
            raise ValueError('Source bundle identifier differs')
        shutil.copy2(args.profile, app / 'embedded.mobileprovision')
        main_executable = app / info['CFBundleExecutable']
        for path in sorted(code_files(app), key=lambda path: len(path.parts), reverse=True):
            if path != main_executable:
                run('codesign', '--force', '--sign', args.identity, '--timestamp=none', str(path))
        for framework in sorted(app.rglob('*.framework'), key=lambda path: len(path.parts), reverse=True):
            run('codesign', '--force', '--sign', args.identity, '--timestamp=none', str(framework))
        run('codesign', '--force', '--sign', args.identity, '--entitlements', str(args.entitlements),
            '--timestamp=none', str(app))
    report = verify(app, args.expected_bundle, args.expected_team)
    if args.report:
        args.report.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({key: value for key, value in report.items() if key != 'artifact_tree'}, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (RuntimeError, ValueError, OSError, KeyError, plistlib.InvalidFileException) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
