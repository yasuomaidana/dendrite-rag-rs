#!/usr/bin/env python3
"""Fails when a BUSL-1.1 dependency has no public source repository.

Policy (DRAG-129): `deny.toml` lets BUSL-1.1 crates through, because cargo-deny cannot see whether the
code behind a licence is open to read. This check closes that gap. A BUSL-1.1 crate is accepted only when the
GitHub repository named in its manifest is public. oxirag and oxillama went private when COOLJAPAN relicensed
them, so they fail here (and are also banned by name in `deny.toml`).

Usage: python3 -I scripts/check-busl-public-repos.py   (needs network access to api.github.com)
"""

import json
import re
import subprocess
import sys
import urllib.error
import urllib.request

# Licences read from the manifest, or from the LICENSE file for crates that declare none.
LICENSE_FILE_MARKER = "Business Source License 1.1"


def metadata():
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features"],
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(out.stdout)


def is_busl(package):
    if "BUSL-1.1" in (package.get("license") or ""):
        return True
    if package.get("license"):
        return False
    # No licence field: look at the LICENSE file shipped with the crate.
    manifest_dir = package["manifest_path"].rsplit("/", 1)[0]
    try:
        with open(f"{manifest_dir}/LICENSE", encoding="utf-8", errors="replace") as f:
            return LICENSE_FILE_MARKER in f.read(2000)
    except OSError:
        return False


def github_slug(repository):
    match = re.match(r"https?://github\.com/([^/]+/[^/#?]+?)(?:\.git)?/?$", repository or "")
    return match.group(1) if match else None


def is_public(slug):
    request = urllib.request.Request(
        f"https://api.github.com/repos/{slug}", headers={"User-Agent": "busl-public-repo-check"}
    )
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return json.load(response).get("private") is False
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise


def main():
    failures = []
    repositories = {}
    for package in metadata()["packages"]:
        if package["source"] is None or not is_busl(package):
            continue
        repositories.setdefault(package.get("repository"), []).append(package["name"])

    for repository, names in sorted(repositories.items(), key=lambda item: str(item[0])):
        slug = github_slug(repository)
        if slug is None:
            failures.append(f"{', '.join(names)}: BUSL-1.1 and no GitHub repository ({repository!r})")
        elif not is_public(slug):
            failures.append(f"{', '.join(names)}: BUSL-1.1 and the repository {slug} is private or gone")
        else:
            print(f"ok  {slug} is public ({len(names)} crate(s) under BUSL-1.1)")

    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
