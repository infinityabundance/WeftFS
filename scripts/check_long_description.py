#!/usr/bin/env python3
"""Fail closed when the wheel long description describes its own publication.

`python/README.md` is baked into the wheel as the PyPI long description at build
time. An artifact cannot truthfully describe its own registry state: a sentence
such as "PyPI serves <version>", "this version is not uploaded", or "the wheel
sha256 is <digest>" is false the moment the artifact is uploaded, and it also
makes every rebuild produce different bytes. This checker rejects that whole
class of statement instead of one wording of it.

Rules (all case-insensitive unless noted):

  pinned-install          `pip install weftcodec==<version>` (also inside a
                          quoted extra such as `"weftcodec[torch]==<version>"`)
  local-artifact-install  `pip install dist/...` / `pip install ./...whl`
  wheel-path-install      any `pip install` that names a `.whl` file
  registry-serves         any claim about what PyPI or crates.io serves/hosts
  upload-state            "not uploaded", "not published", "no token", ...
  package-version         a version literal bound to `weftcodec`, e.g.
                          `weftcodec 1.4.2`, `weftcodec==1.4.2`,
                          `weftcodec-1.4.2-py3-none-any.whl`, `.../weftcodec/1.4.2/`
  digest-literal          any literal 64-hex SHA-256 value (case-sensitive)

It also requires the three stable statements that replace that class: an
unpinned `pip install weftcodec`, the PyPI project URL, and the crates.io crate
URL. Those make the removal of a version-specific block visible rather than
silent.

Host-only, standard library only, no network. `--file -` reads stdin, which is
how the court demonstrates the old text failing.
"""

from __future__ import annotations

import argparse
import os
import re
import sys

TARGET = "python/README.md"

RULES = [
    (
        "pinned-install",
        re.compile(
            r"pip\s+install\s+(?:[A-Za-z0-9_.:/\\-]*/)?weftcodec"
            r"(?:\[[^\]\n]*\])?\s*==",
            re.IGNORECASE,
        ),
        "a version-pinned `pip install weftcodec==<version>`",
    ),
    (
        "local-artifact-install",
        re.compile(
            r"pip\s+install\s+(?:[\"'])?(?:\./)?(?:python/)?dist/[^\s\"'`)]*",
            re.IGNORECASE,
        ),
        "installing a local build artifact (`pip install dist/...`)",
    ),
    (
        "wheel-path-install",
        re.compile(r"pip\s+install\b[^\n]*\.whl\b", re.IGNORECASE),
        "installing a wheel file path",
    ),
    (
        "registry-serves",
        re.compile(
            r"\b(?:PyPI|crates\.io)\b[\s\S]{0,40}?"
            r"\b(?:serves|hosts|carries|ships|holds|still\s+serves)\b",
            re.IGNORECASE,
        ),
        "a claim about what a registry serves",
    ),
    (
        "upload-state",
        re.compile(
            r"\b(?:not\s+uploaded|not\s+published|nothing\s+was\s+uploaded"
            r"|pending\s+upload|until\s+the\s+upload|upload\s+is\s+unblocked"
            r"|token\s+does\s+not\s+exist|no\s+token\s+(?:is|was)\s+available"
            r"|fails\s+until\s+the\s+upload"
            r"|(?:is|are)\s+not\s+(?:yet\s+)?on\s+(?:PyPI|crates\.io))\b",
            re.IGNORECASE,
        ),
        "a claim about whether this artifact is uploaded",
    ),
    (
        "package-version",
        re.compile(
            r"\bweftcodec(?:\[[^\]\n]*\])?"
            r"(?:\s*==\s*|\s*-\s*|\s+|\s*/\s*)\d+\.\d+\.\d+\b",
            re.IGNORECASE,
        ),
        "a version literal bound to `weftcodec`",
    ),
    (
        "digest-literal",
        re.compile(r"\b[0-9a-fA-F]{64}\b"),
        "a 64-hex digest literal",
    ),
]

# The stable, version-free statements the long description must keep.
REQUIRED = [
    ("unpinned-install", re.compile(r"pip\s+install\s+weftcodec\b", re.IGNORECASE),
     "an unpinned `pip install weftcodec`"),
    ("pypi-project-url", re.compile(r"https://pypi\.org/project/weftcodec/?"),
     "the PyPI project URL"),
    ("crates-crate-url", re.compile(r"https://crates\.io/crates/weftcodec\b"),
     "the crates.io crate URL"),
]


def read_text(path):
    if path == "-":
        return sys.stdin.read()
    with open(path, "r", encoding="utf-8") as handle:
        return handle.read()


def check(text, label):
    problems = []
    for rule_id, pattern, description in RULES:
        for match in pattern.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            snippet = " ".join(match.group(0).split())
            problems.append(
                "%s:%d: %s: %s (found %r)"
                % (label, line, rule_id, description, snippet)
            )
    for rule_id, pattern, description in REQUIRED:
        if not pattern.search(text):
            problems.append("%s: missing-%s: %s is required" % (label, rule_id, description))
    return problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repository root")
    parser.add_argument(
        "--file",
        default=None,
        help="file to check (default: <root>/%s); '-' reads stdin" % TARGET,
    )
    args = parser.parse_args(argv)

    root = os.path.abspath(args.root)
    if args.file is not None:
        path = args.file
        label = "<stdin>" if path == "-" else path
    else:
        path = os.path.join(root, TARGET)
        label = TARGET
        if not os.path.isfile(path):
            print(
                "long_description: missing %s; create it or pass --file" % TARGET,
                file=sys.stderr,
            )
            return 1

    try:
        text = read_text(path)
    except OSError as exc:
        print("long_description: cannot read %s: %s" % (label, exc), file=sys.stderr)
        return 1

    problems = check(text, label)
    if problems:
        for line in problems:
            print(line, file=sys.stderr)
        print(
            "long_description: FAIL (%d violation(s) in %s)" % (len(problems), label),
            file=sys.stderr,
        )
        return 1
    print(
        "long_description: OK (%s, %d lines, %d rules, %d required statements)"
        % (label, text.count("\n") + 1, len(RULES), len(REQUIRED))
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
