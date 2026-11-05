#!/usr/bin/env python3
"""Fetch a real public checkpoint file and verify it against the HF LFS pointer.

The Hugging Face `resolve` URL redirects to a CDN; urllib follows redirects. We
ask the model API's tree endpoint for the LFS size and sha256 oid, so the
verification target comes from the hub metadata and not from the download
itself. Downloads are resumable through a `.part` file and are renamed only
after size and SHA-256 match.

This script is run inside a court; it needs no third-party packages.

Usage:
  python3 bench/scripts/real_model_fetch.py \
      --repo Qwen/Qwen2.5-0.5B --file model.safetensors \
      --dest .docker-cache/real-corpus/Qwen2.5-0.5B.safetensors

`--revision` selects a branch/tag/commit (for example the Pythia `stepN`
branches); the LFS pointer is read from that revision's tree, so verification
always targets the hub metadata for the exact revision that was downloaded.
"""

import argparse
import hashlib
import json
import os
import sys
import time
import urllib.request

HF = "https://huggingface.co"


def api_tree(repo, revision="main"):
    url = f"{HF}/api/models/{repo}/tree/{revision}?recursive=false"
    with urllib.request.urlopen(url, timeout=60) as response:
        return json.load(response)


def entry_for(repo, name, revision="main"):
    for item in api_tree(repo, revision):
        if item.get("type") == "file" and item.get("path") == name:
            lfs = item.get("lfs") or {}
            return {
                "path": name,
                "size": item.get("size"),
                "lfs_size": lfs.get("size"),
                "sha256": lfs.get("oid"),
                "xet_hash": item.get("xetHash"),
            }
    raise SystemExit(f"file {name!r} not found in {repo}")


def download(url, dest, expected_size):
    part = dest + ".part"
    have = os.path.getsize(part) if os.path.exists(part) else 0
    if have > expected_size:
        os.unlink(part)
        have = 0
    headers = {"User-Agent": "weftfs-real-model-bench/1"}
    if have:
        headers["Range"] = f"bytes={have}-"
    request = urllib.request.Request(url, headers=headers)
    mode = "ab" if have else "wb"
    started = time.time()
    transferred = 0
    last_report = started
    with urllib.request.urlopen(request, timeout=120) as response:
        if have and response.status != 206:
            # Server ignored the range request; restart from zero.
            have = 0
            mode = "wb"
        with open(part, mode) as out:
            while True:
                block = response.read(1 << 20)
                if not block:
                    break
                out.write(block)
                transferred += len(block)
                now = time.time()
                if now - last_report >= 5.0:
                    done = have + transferred
                    rate = transferred / max(now - started, 1e-9) / (1 << 20)
                    print(
                        f"  {done}/{expected_size} bytes "
                        f"({100.0 * done / expected_size:.1f}%) {rate:.1f} MiB/s",
                        flush=True,
                    )
                    last_report = now
    elapsed = time.time() - started
    return part, elapsed, have + transferred


def sha256_file(path, chunk=1 << 20):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        while True:
            block = handle.read(chunk)
            if not block:
                break
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--file", required=True)
    parser.add_argument("--dest", required=True)
    parser.add_argument("--revision", default="main")
    args = parser.parse_args()

    os.makedirs(os.path.dirname(os.path.abspath(args.dest)), exist_ok=True)
    meta = entry_for(args.repo, args.file, args.revision)
    expected = meta["lfs_size"]
    if meta["size"] != expected or not meta["sha256"]:
        raise SystemExit(f"missing LFS pointer metadata for {args.repo}/{args.file}: {meta}")
    if os.path.exists(args.dest) and os.path.getsize(args.dest) == expected:
        digest = sha256_file(args.dest)
        if digest == meta["sha256"]:
            print(f"{args.dest}: already present, sha256 ok")
            print(json.dumps({**meta, "repo": args.repo, "verified": True}))
            return
        raise SystemExit(f"{args.dest}: size matches but sha256 is {digest}")

    url = f"{HF}/{args.repo}/resolve/{args.revision}/{args.file}"
    part, elapsed, downloaded = download(url, args.dest, expected)
    actual = os.path.getsize(part)
    if actual != expected:
        raise SystemExit(f"{part}: expected {expected} bytes, got {actual}")
    digest = sha256_file(part)
    if digest != meta["sha256"]:
        raise SystemExit(f"{part}: sha256 {digest} != declared {meta['sha256']}")
    os.replace(part, args.dest)
    print(f"{args.dest}: {expected} bytes verified in {elapsed:.1f}s")
    print(
        json.dumps(
            {
                "repo": args.repo,
                "file": args.file,
                "revision": args.revision,
                "path": args.dest,
                "size": expected,
                "sha256": digest,
                "lfs_sha256": meta["sha256"],
                "xet_hash": meta.get("xet_hash"),
                "verified": True,
                "download_seconds": round(elapsed, 3),
                "downloaded_bytes": downloaded,
            }
        )
    )


if __name__ == "__main__":
    sys.exit(main())
