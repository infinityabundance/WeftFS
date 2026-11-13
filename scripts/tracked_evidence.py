#!/usr/bin/env python3
"""Tracked-file checks for the host-only evidence guards.

A file that exists only in the author's working tree is **not evidence**: a
fresh checkout does not have it, so a guard can pass locally and fail in CI, or
cite something a clone cannot verify. This module answers one question for
`docs/claims.json`, `docs/perf-numbers.json` and `conformance/manifest.json`:

    is this path committed to the repository that owns it?

The **nearest enclosing repository** decides. The three subprojects
(`WeftCodec/`, `WeftCodec-Go/`, `WeftFS-Go/`) are independent git repositories
and the program home records only a gitlink for each, so `git ls-files` at the
parent root cannot see their contents. Walking up to the nearest `.git` also
does the right thing for a git worktree, where `.git` is a file.

Outside a git work tree the answer is `None` ("unknown"): an exported tarball,
the conformance bundle, or a vendored copy must still be verifiable, so callers
skip the check with a printed note rather than failing closed on the absence of
git.

Standard library only, no network, no writes.
"""

from __future__ import annotations

import os
import subprocess


class TrackedEvidence:
    """Per-repository index of tracked paths, built once per repository."""

    def __init__(self, root="."):
        self.root = os.path.abspath(root)
        self._repos = {}

    # -- repository resolution --------------------------------------------

    def repo_of(self, path):
        """The nearest enclosing repository root for `path`, or None."""
        current = os.path.dirname(os.path.abspath(path))
        while True:
            if os.path.exists(os.path.join(current, ".git")):
                return current
            parent = os.path.dirname(current)
            if parent == current:
                return None
            current = parent

    def _index(self, repo):
        if repo in self._repos:
            return self._repos[repo]
        try:
            proc = subprocess.run(
                ["git", "-C", repo, "ls-files", "-z", "--", "."],
                capture_output=True,
                check=False,
            )
        except OSError:
            self._repos[repo] = None
            return None
        if proc.returncode != 0:
            self._repos[repo] = None
            return None
        names = proc.stdout.decode("utf-8", "surrogateescape").split("\0")
        self._repos[repo] = {name for name in names if name}
        return self._repos[repo]

    # -- queries -----------------------------------------------------------

    def tracked(self, path):
        """True when `path` is tracked, False when it is not, None when unknown.

        `None` means the path is not inside a git work tree (or git refused to
        answer); the caller decides whether that is acceptable.
        """
        full = path if os.path.isabs(path) else os.path.join(self.root, path)
        full = os.path.abspath(full)
        repo = self.repo_of(full)
        if repo is None:
            return None
        names = self._index(repo)
        if names is None:
            return None
        return os.path.relpath(full, repo) in names

    def describe_repo(self, path):
        full = path if os.path.isabs(path) else os.path.join(self.root, path)
        repo = self.repo_of(os.path.abspath(full))
        return repo or "(no repository)"

    def tracked_markdown(self):
        """Repo-relative `*.md` paths tracked in the root's own repository.

        `None` when the root is not inside a git work tree.
        """
        names = self._index(self.root)
        if names is None:
            return None
        return sorted(name for name in names if name.endswith(".md"))


def require_tracked(index, path, what="evidence"):
    """Raise `EvidenceError` when `path` exists but is not committed.

    Returns True when the check ran, False when it was skipped because the path
    is not inside a git work tree.
    """
    verdict = index.tracked(path)
    if verdict is None:
        return False
    if not verdict:
        raise EvidenceError(
            "%s: %s exists but is not tracked by git in %s; an untracked file is "
            "not evidence (a fresh checkout would not have it)"
            % (what, path, index.describe_repo(path))
        )
    return True


class EvidenceError(Exception):
    pass
