#!/usr/bin/env python3
"""Fail if any crates.io package in the given Cargo.lock files is too new.

Policy: every locked crates.io version MUST have been published at least
MIN_DAYS days (default 14) before the check runs. A very new release has had
little time for the community to notice a broken or malicious upload.

Publish times come from the `pubtime` field of the crates.io sparse index
(https://index.crates.io). Versions without `pubtime` are reported as unknown
and fail the check.

Usage:
    scripts/check-dep-age.py [--min-days N] [--allow NAME@VERSION ...] LOCKFILE...

Exit status: 0 if all versions pass, 1 if any is too new or unknown,
2 on usage or network errors.
"""

import argparse
import concurrent.futures
import datetime as dt
import json
import sys
import tomllib
import urllib.error
import urllib.request

INDEX = "https://index.crates.io"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
USER_AGENT = "impromptu-wifi-ci dependency-age-check"


def index_path(name: str) -> str:
    """Path of a crate's file in the sparse index (Cargo's layout)."""
    n = name.lower()
    if len(n) <= 2:
        return f"{len(n)}/{n}"
    if len(n) == 3:
        return f"3/{n[0]}/{n}"
    return f"{n[0:2]}/{n[2:4]}/{n}"


def fetch_pubtimes(name: str) -> dict[str, str | None]:
    req = urllib.request.Request(
        f"{INDEX}/{index_path(name)}", headers={"User-Agent": USER_AGENT}
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        lines = resp.read().decode().splitlines()
    out = {}
    for line in lines:
        if line.strip():
            entry = json.loads(line)
            out[entry["vers"]] = entry.get("pubtime")
    return out


def locked_packages(paths: list[str]) -> set[tuple[str, str]]:
    pkgs = set()
    for path in paths:
        with open(path, "rb") as f:
            lock = tomllib.load(f)
        for p in lock.get("package", []):
            if p.get("source") == CRATES_IO:
                pkgs.add((p["name"], p["version"]))
    return pkgs


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("lockfiles", nargs="+", metavar="LOCKFILE")
    ap.add_argument("--min-days", type=int, default=14)
    ap.add_argument(
        "--allow",
        action="append",
        default=[],
        metavar="NAME@VERSION",
        help="exempt one version, for example a security fix",
    )
    args = ap.parse_args()

    allowed = set(args.allow)
    pkgs = locked_packages(args.lockfiles)
    names = sorted({n for n, _ in pkgs})
    now = dt.datetime.now(dt.timezone.utc)
    cutoff = now - dt.timedelta(days=args.min_days)

    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            times = dict(zip(names, pool.map(fetch_pubtimes, names)))
    except (urllib.error.URLError, TimeoutError) as e:
        print(f"error: crates.io index request failed: {e}", file=sys.stderr)
        return 2

    failures = []
    for name, version in sorted(pkgs):
        if f"{name}@{version}" in allowed:
            continue
        stamp = times[name].get(version)
        if stamp is None:
            failures.append(f"{name}@{version}: publish time unknown")
            continue
        published = dt.datetime.fromisoformat(stamp.replace("Z", "+00:00"))
        if published > cutoff:
            age = (now - published).days
            failures.append(
                f"{name}@{version}: published {stamp} ({age} days ago)"
            )

    if failures:
        print(
            f"{len(failures)} locked version(s) are newer than "
            f"{args.min_days} days or have no publish time:"
        )
        for f in failures:
            print(f"  {f}")
        print(
            "Pin an older version with `cargo update -p NAME --precise VERSION`, "
            "or pass --allow NAME@VERSION with a reason in the workflow."
        )
        return 1

    print(f"ok: {len(pkgs)} crates.io versions are at least {args.min_days} days old")
    return 0


if __name__ == "__main__":
    sys.exit(main())
