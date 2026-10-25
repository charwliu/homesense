#!/usr/bin/env python3
"""Check tracked/staged public files without printing credential contents."""
from __future__ import annotations
import argparse
import ipaddress
from pathlib import PurePosixPath
import re
import subprocess

PRIVATE_PREFIXES = (
    'config/import/', 'config/live/', 'config/scenes/', 'docs/evidence/',
    'crates/homesense-import/tests/fixtures/study-events/',
    'crates/homesensed/tests/fixtures/site-captures/',
)
PRIVATE_FILES = {
    'reference/manifest.json', 'reference/control4/hb-device-map.json',
    'reference/control4/hb-module-addresses.txt',
    'deployment/network/huawei-s5720s-ipv6-pbr-production.cli.example',
    'deployment/network/huawei-s5720s-ipv6-pbr.cli.example',
    'deployment/network/huawei-s5720s-ipv6-source-pbr.cli.example',
    'deployment/network/ipv6-pbr-hairpin-a.nft.example',
    'deployment/network/ipv6-pbr-hairpin-b.nft.example',
}
PRIVATE_SUFFIXES = ('.key', '.p12', '.pfx', '.mobileprovision', '.sqlite',
                    '.sqlite-wal', '.sqlite-shm', '.pcap', '.pcapng', '.log')
SECRET_PATTERNS = {
    'private key': re.compile(rb'-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----'),
    'GitHub credential': re.compile(rb'\b(?:gh[pousr]_[A-Za-z0-9]{25,}|github_pat_[A-Za-z0-9_]{35,})\b'),
    'AWS credential': re.compile(rb'\b(?:AKIA|ASIA)[A-Z0-9]{16}\b'),
    'JWT literal': re.compile(rb'\beyJ[A-Za-z0-9_-]{12,}\.[A-Za-z0-9_-]{12,}\.[A-Za-z0-9_-]{20,}\b'),
}


def private_path(path: str) -> bool:
    parts = PurePosixPath(path).parts
    return (path.startswith(PRIVATE_PREFIXES) or path in PRIVATE_FILES
            or bool({'private', 'secrets', 'evidence'} & set(parts))
            or path.endswith(PRIVATE_SUFFIXES)
            or PurePosixPath(path).name == '.env'
            or PurePosixPath(path).name.startswith('.env.')
            or bool(re.fullmatch(r'deployment/[^/]+/\d[^/]*\.json', path))
            or bool(re.fullmatch(r'config/test/tcp485-vlan3\..*\.json', path)))


def content_findings(path: str, data: bytes) -> list[str]:
    reasons = [label for label, pattern in SECRET_PATTERNS.items() if pattern.search(data)]
    # Public network templates use documentation prefixes or explicit placeholders.
    if path.startswith('deployment/') and path.endswith('.example'):
        for match in re.finditer(rb'(?i)(?<![\w:])[23][a-f0-9]{3}:[a-f0-9:]+', data):
            try:
                address = ipaddress.ip_address(match[0].decode('ascii'))
            except ValueError:
                continue
            if address not in ipaddress.ip_network('2001:db8::/32'):
                reasons.append('non-documentation IPv6 address in template')
                break
    return reasons


def check_index(root: str) -> list[tuple[str, str]]:
    rows = subprocess.check_output(['git', '-C', root, 'ls-files', '--stage', '-z']).split(b'\0')
    records = []
    for row in filter(None, rows):
        metadata, raw_path = row.split(b'\t', 1)
        mode, oid, stage = metadata.decode().split()
        path = raw_path.decode('utf-8', 'surrogateescape')
        if stage != '0':
            records.append((path, None, 'unmerged index entry'))
        elif mode == '160000':
            records.append((path, None, 'submodule requires separate review'))
        else:
            records.append((path, oid, None))
    findings = []
    proc = subprocess.Popen(['git', '-C', root, 'cat-file', '--batch'],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    try:
        for path, oid, error in records:
            if error:
                findings.append((path, error))
                continue
            if private_path(path):
                findings.append((path, 'private artifact path'))
            proc.stdin.write((oid + '\n').encode())
            proc.stdin.flush()
            header = proc.stdout.readline().split()
            if len(header) != 3 or header[1] != b'blob':
                raise RuntimeError('unexpected Git object response')
            data = proc.stdout.read(int(header[2]))
            if proc.stdout.read(1) != b'\n':
                raise RuntimeError('truncated Git object')
            findings.extend((path, reason) for reason in content_findings(path, data))
    finally:
        proc.stdin.close()
        proc.stdout.close()
        proc.wait()
    if proc.returncode:
        raise RuntimeError('Git object scan failed')
    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', default='.')
    args = parser.parse_args()
    findings = check_index(args.root)
    for path, reason in findings:
        print(f'{path}: {reason}')
    print(f'Public index scan: {len(findings)} finding(s); values are never printed.')
    return bool(findings)


if __name__ == '__main__':
    raise SystemExit(main())
