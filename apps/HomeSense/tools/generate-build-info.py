#!/usr/bin/env python3
"""Generate the app Info.plist from the current, complete Git checkout."""
import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import re
import subprocess


def git(project, *args):
    env = dict(os.environ, GIT_OPTIONAL_LOCKS="0", GIT_CONFIG_NOSYSTEM="1",
               GIT_CONFIG_GLOBAL=os.devnull)
    return subprocess.check_output(["/usr/bin/git", "-C", str(project), *args], env=env)


def identity(project):
    project = project.resolve()
    root = Path(os.fsdecode(git(project, "rev-parse", "--show-toplevel")).strip())
    scope = project.relative_to(root).as_posix()
    if git(project, "rev-parse", "--is-shallow-repository").strip() != b"false":
        raise ValueError("Build numbering requires a complete Git history")
    revision = git(project, "rev-parse", "HEAD").decode().strip()
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("Unexpected Git revision")
    count = git(project, "rev-list", "--count", "HEAD").decode().strip()
    if not count.isascii() or not count.isdigit() or int(count) < 1:
        raise ValueError("Unexpected Git commit count")
    dirty = bool(git(root, "status", "--porcelain=v1", "--untracked-files=all", "--", scope))
    names = git(root, "ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", scope)
    digest = hashlib.sha256()
    for name in sorted(set(names.split(b"\0")) - {b""}):
        path = root / os.fsdecode(name)
        if path.is_symlink():
            raise ValueError("App source must not contain symbolic links")
        digest.update(name + b"\0")
        digest.update(hashlib.sha256(path.read_bytes()).digest() if path.exists() else b"deleted")
    # Reject a checkout changed while collecting its identity.
    if git(project, "rev-parse", "HEAD").decode().strip() != revision:
        raise ValueError("Git HEAD changed during build metadata generation")
    return {"CFBundleVersion": count, "HomeSenseGitCommit": revision,
            "HomeSenseGitDirty": dirty, "HomeSenseSourceFingerprint": digest.hexdigest()}


def generate(project, template, output):
    metadata = identity(project)
    info = plistlib.loads(template.read_bytes())
    info.update(metadata)
    data = plistlib.dumps(info)
    output.parent.mkdir(parents=True, exist_ok=True)
    if not output.exists() or output.read_bytes() != data:
        output.write_bytes(data)
    return metadata


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", required=True, type=Path)
    parser.add_argument("--template", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    metadata = generate(args.project, args.template, args.output)
    print("HomeSense source " + metadata["HomeSenseGitCommit"][:7]
          + (" +dirty" if metadata["HomeSenseGitDirty"] else "")
          + "; bundle build " + metadata["CFBundleVersion"])


if __name__ == "__main__":
    main()
