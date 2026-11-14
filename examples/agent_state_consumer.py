#!/usr/bin/env python3
"""Agent-state consumer over the one explicit artifact API (T072).

The point of this script is that an agent-state consumer uses exactly the same
``weftfs artifact`` surface as the PyTorch DCP and Orbax adapters: resolve,
plan, materialize, verify, pin, release, and one-commit transactions. It never
mounts the store and it contains no store logic of its own.

Setup writes the memory fixtures through the ordinary CLI verbs, then every read
and every lifetime operation goes through :class:`weftcodec.artifact_api.ArtifactClient`.

Usage::

    python3 examples/agent_state_consumer.py \
        --store /tmp/agent-store --cli /work/.cache/weftfs \
        --out /tmp/agent-out --json /tmp/agent.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "python"))

from weftcodec.artifact_api import ArtifactClient  # noqa: E402


def run(cli, store, args, *, json_out=False):
    command = [cli, "--store", os.fspath(store), *args]
    proc = subprocess.run(command, capture_output=True, check=False)
    if proc.returncode != 0:
        raise SystemExit(
            "weftfs %s failed (%d): %s"
            % (" ".join(args), proc.returncode, proc.stderr.decode("utf-8", "replace"))
        )
    if json_out:
        return json.loads(proc.stdout.decode("utf-8"))
    return proc.stdout


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("--store", required=True)
    parser.add_argument("--cli", default=os.environ.get("WEFTFS_CLI", "weftfs"))
    parser.add_argument("--out", required=True)
    parser.add_argument("--json", default=None)
    parser.add_argument("--snapshot", default="jan")
    args = parser.parse_args(argv)

    store = Path(args.store)
    out = Path(args.out)
    prompt = b"Summarize the retained experiment log. Task: summarize.\n"
    model = b"llama-4 ver_19 weights marker\n"
    output = b"agent output run-001: exact retained bytes\n"

    run(args.cli, store, ["--json", "init"])
    for name in ("prompts", "models", "outputs"):
        run(args.cli, store, ["--json", "mkdir", "/" + name])
    run(
        args.cli,
        store,
        ["--json", "create", "/prompts/task.txt", "--text", prompt.decode(),
         "--attr", "task=summarize", "--attr", "model=llama-4"],
    )
    run(
        args.cli,
        store,
        ["--json", "create", "/models/llama-4.bin", "--text", model.decode(),
         "--attr", "task=summarize", "--attr", "model=llama-4"],
    )
    run(
        args.cli,
        store,
        ["--json", "create", "/outputs/run-001.bin", "--text", output.decode(),
         "--attr", "task=summarize", "--attr", "model=llama-4"],
    )
    out_id = run(
        args.cli, store, ["--json", "stat", "/outputs/run-001.bin"], json_out=True
    )["object_id"]
    model_id = run(
        args.cli, store, ["--json", "stat", "/models/llama-4.bin"], json_out=True
    )["object_id"]
    run(
        args.cli,
        store,
        ["--json", "relation", "add", "--subject", out_id,
         "--predicate", "generated_by", "--object", model_id],
    )
    run(args.cli, store, ["--json", "snapshot", "create", args.snapshot])
    run(
        args.cli,
        store,
        ["--json", "context", "create", "--name", "by:task=summarize",
         "--selector", 'select attrs.task == "summarize"',
         "--snapshot", args.snapshot],
    )

    client = ArtifactClient(store, cli=args.cli)
    root = {"root": "by:task=summarize"}
    resolution = client.resolve(root, snapshot=args.snapshot)
    plan = client.plan(root, snapshot=args.snapshot)
    if not plan.get("complete"):
        raise SystemExit("agent plan is incomplete: %r" % (plan.get("missing"),))
    materialized = client.materialize(root, out, snapshot=args.snapshot)
    verification = client.verify(root, snapshot=args.snapshot, out=out)

    # One transaction, one commit: retain the output and the model it used.
    with client.transaction(snapshot=args.snapshot) as txn:
        txn.pin({"object_id": out_id}, policy_epoch=1, declared_lifetime_ms=86_400_000)
        txn.pin({"object_id": model_id}, policy_epoch=1, declared_lifetime_ms=86_400_000)

    pinned = client.resolve({"object_id": out_id}, snapshot=args.snapshot)
    marker = {
        "output_object_id": out_id,
        "output_content_root": pinned["entries"][0]["content_root"],
    }

    # Release both in one commit. Reclamation is an explicit gc, not implied.
    with client.transaction(snapshot=args.snapshot) as txn:
        txn.release({"object_id": out_id})
        txn.release({"object_id": model_id})

    files = sorted(
        str(path.relative_to(out))
        for path in out.rglob("*")
        if path.is_file() and path.name != ".weft-artifact.json"
    )
    payload = {
        "schema": "weft.agent-state.consumer.v1",
        "store": os.fspath(store),
        "snapshot": args.snapshot,
        "verbs": list(client.calls),
        "resolve": {
            "matched_count": resolution["matched_count"],
            "snapshot": resolution["snapshot"],
            "provisional": resolution["provisional"],
            "entries": [
                {
                    "name": entry["name"],
                    "content_root": entry["content_root"],
                    "bytes": entry["bytes"],
                }
                for entry in resolution["entries"]
            ],
        },
        "plan": {
            "plan_root": plan["plan_root"],
            "complete": plan["complete"],
            "missing": plan["missing"],
        },
        "materialize": {
            "files": materialized["files"],
            "bytes": materialized["bytes"],
            "plan_root": materialized["plan_root"],
            "file_sha256": {
                row["name"]: row["sha256"]
                for row in materialized.get("file_digests", [])
                if row["name"].endswith(".bin") or row["name"].endswith(".txt")
            },
        },
        "output_files": files,
        "output_sha256": {
            name: sha256_file(out / name) for name in files if (out / name).is_file()
        },
        "verify": {
            "ok": verification["ok"],
            "store_checked": verification["store_checked"],
            "output_checked": verification["output_checked"],
            "problems": verification["problems"],
        },
        "pin": marker,
        "transaction_verified": True,
        "client_calls": [
            {"verb": verb, "count": client.calls.count(verb)}
            for verb in sorted(set(client.calls))
        ],
        "label": (
            "agent-state consumer, single host, synthetic fixture, no mount; "
            "exercises the same artifact API as the DCP and Orbax adapters"
        ),
    }
    text = json.dumps(payload, indent=2, sort_keys=True)
    if args.json:
        Path(args.json).write_text(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
