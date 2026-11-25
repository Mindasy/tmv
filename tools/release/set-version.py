#!/usr/bin/env python3
"""Set a checkout's package version from a release tag, without committing it."""
import argparse
import pathlib
import re

TAG = re.compile(r'v((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))(?:-[A-Za-z0-9][A-Za-z0-9._-]*)?')


def replace_once(text, pattern, replacement):
    result, count = re.subn(pattern, replacement, text, flags=re.MULTILINE)
    if count != 1:
        raise ValueError('expected exactly one match: ' + pattern)
    return result


def set_version(root, tag):
    match = TAG.fullmatch(tag)
    if not match:
        raise ValueError('expected vX.Y.Z or vX.Y.Z-SUFFIX (for example v0.1.3-20261125)')
    version = match[1]
    # Prepare and validate every edit before writing any file.
    files = {}
    manifest = (root / 'Cargo.toml').read_text()
    pattern = r'(\[workspace\.package\]\s*\nversion\s*=\s*)"([^"\n]+)"'
    current = re.search(pattern, manifest)
    if not current:
        raise ValueError('workspace package version missing')
    previous = current[2]
    files['Cargo.toml'] = replace_once(manifest, pattern, lambda m: m[1] + '"' + version + '"')
    names = set()
    for path in (root / 'crates').glob('*/Cargo.toml'):
        name = re.search(r'^name\s*=\s*"([^"]+)"', path.read_text(), re.MULTILINE)
        if name:
            names.add(name[1])
    blocks = (root / 'Cargo.lock').read_text().split('[[package]]')
    updated = set()
    for index, block in enumerate(blocks[1:], 1):
        name = re.search(r'^name = "([^"]+)"', block, re.MULTILINE)
        if name and name[1] in names and not re.search(r'^source\s*=', block, re.MULTILINE):
            blocks[index] = replace_once(block, r'^version = "[^"]+"', 'version = "' + version + '"')
            updated.add(name[1])
    if not names or updated != names:
        raise ValueError('workspace packages do not match Cargo.lock')
    files['Cargo.lock'] = '[[package]]'.join(blocks)
    spec = (root / 'tmv.spec').read_text()
    spec = replace_once(spec, r'^Version:\s*\S+\s*$', 'Version:        ' + version)
    spec = replace_once(spec, r'^%global release_tag \S+$', '%global release_tag ' + tag)
    files['tmv.spec'] = spec
    # Keep installed documentation/examples consistent with the temporary version.
    for name in ['README.md', 'tmv.1', 'RELEASE-NOTES.md', 'tools/release/README.md']:
        files[name] = (root / name).read_text().replace(previous, version)
    notes = re.sub(r'^Release tag: .*\n\n', '', files['RELEASE-NOTES.md'], flags=re.MULTILINE)
    files['RELEASE-NOTES.md'] = replace_once(notes, r'^# TMV .+$', '# TMV ' + version + '\n\nRelease tag: ' + tag)
    for name, text in files.items():
        (root / name).write_text(text)
    return version


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tag')
    parser.add_argument('--root', type=pathlib.Path, default=pathlib.Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    try:
        print(set_version(args.root, args.tag))
    except ValueError as error:
        parser.error(str(error))
