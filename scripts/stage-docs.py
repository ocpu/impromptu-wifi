#!/usr/bin/env python3
"""Stage the documentation for mdBook.

Copies docs/ to the root of OUT, and the READMEs the documentation links to
at their repository paths; rewrites relative links so that links between
staged pages stay relative and every other link points at the file on
GitHub; and writes OUT/SUMMARY.md in the order of docs/README.md.

Usage: stage-docs.py [--check] OUT

--check exits with status 1 and lists links whose target does not exist in
the repository.
"""

import argparse
import os
import pathlib
import re
import shutil
import sys

GITHUB = "https://github.com/ocpu/impromptu-wifi"
# Markdown outside docs/ that the documentation links to.
EXTRA = [
    "examples/esp32s3/README.md",
    "examples/raspberry-pi/README.md",
    "crates/impromptu-wifi-linux/dist/README.md",
]

LINK = re.compile(r"(\]\()([^)\s]+)(\))")
FENCE = re.compile(r"^\s*(```|~~~)")
SCHEME = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")

broken = []


def staged_path(rel):
    """Where repository path `rel` is staged, or None if it isn't."""
    rel = pathlib.PurePosixPath(rel)
    if rel.parts and rel.parts[0] == "docs" and rel.suffix == ".md":
        return pathlib.PurePosixPath(*rel.parts[1:])
    if str(rel) in EXTRA:
        return rel
    return None


def rewrite_link(link, page, repo):
    """Rewrite one link found on repository page `page`."""
    if link.startswith(("#", "/")) or SCHEME.match(link):
        return link
    path, sep, anchor = link.partition("#")
    anchor = sep + anchor
    target = (page.parent / path).resolve()
    try:
        rel = target.relative_to(repo).as_posix()
    except ValueError:
        broken.append((page, link))
        return link
    if not target.exists():
        broken.append((page, link))
    here = staged_path(page.relative_to(repo).as_posix())
    if target.is_dir():
        readme = staged_path(f"{rel}/README.md")
        if readme is not None and here is not None and (target / "README.md").is_file():
            return os.path.relpath(readme, here.parent).replace(os.sep, "/") + anchor
        return f"{GITHUB}/tree/main/{rel}"
    there = staged_path(rel)
    if there is not None and here is not None:
        return os.path.relpath(there, here.parent).replace(os.sep, "/") + anchor
    return f"{GITHUB}/blob/main/{rel}{anchor}"


def rewrite_markdown(text, page, repo):
    """Rewrite the links in `text`, outside fenced code blocks."""
    out, fenced = [], False
    for line in text.splitlines(keepends=True):
        if FENCE.match(line):
            fenced = not fenced
        elif not fenced:
            line = LINK.sub(lambda m: m[1] + rewrite_link(m[2], page, repo) + m[3], line)
        out.append(line)
    return "".join(out)


def summary(repo, pages):
    """SUMMARY.md: sections and links in the order of docs/README.md."""
    index = repo / "docs" / "README.md"
    lines = ["# Summary", "", "[Introduction](README.md)", ""]
    listed = {pathlib.PurePosixPath("README.md")}
    fenced = False
    for line in index.read_text().splitlines():
        if FENCE.match(line):
            fenced = not fenced
            continue
        if fenced:
            continue
        if line.startswith("## "):
            lines += [f"# {line[3:].strip()}", ""]
            continue
        if not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        first = cells[0]
        m = re.search(r"\[([^\]]+)\]\(([^)#\s]+)", first)
        if not m:
            continue
        rel = (index.parent / m[2]).resolve().relative_to(repo).as_posix()
        staged = staged_path(rel)
        if staged is None or staged in listed:
            continue
        title = m[1].replace("`", "")
        if title.isdigit() and len(cells) > 1:
            # Decision records: the number, then the decision.
            title = f"{title}: {cells[1]}".replace("`", "")
        lines.append(f"- [{title}]({staged.as_posix()})")
        listed.add(staged)
    rest = sorted(p for p in pages if p not in listed)
    if rest:
        lines += ["", "# More", ""]
        lines += [f"- [{p.stem}]({p.as_posix()})" for p in rest]
    return "\n".join(lines) + "\n"


def stage(repo, out):
    """Stage the documentation of `repo` into `out`."""
    repo = pathlib.Path(repo).resolve()
    out = pathlib.Path(out)
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    sources = sorted((repo / "docs").rglob("*.md")) + [repo / e for e in EXTRA]
    pages = []
    for src in sources:
        staged = staged_path(src.relative_to(repo).as_posix())
        dst = out / staged
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_text(rewrite_markdown(src.read_text(), src, repo))
        pages.append(staged)
    (out / "SUMMARY.md").write_text(summary(repo, pages))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=pathlib.Path)
    parser.add_argument("--check", action="store_true", help="fail on broken links")
    args = parser.parse_args()
    repo = pathlib.Path(__file__).resolve().parent.parent
    stage(repo, args.out)
    if args.check and broken:
        for page, link in broken:
            print(f"{page.relative_to(repo)}: broken link {link}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
