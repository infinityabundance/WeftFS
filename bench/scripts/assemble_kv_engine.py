#!/usr/bin/env python3
"""Assemble the engine court receipt (subphase X17/T064/T069).

Combines two things that were actually run:

* `--attempts` the engine-install document from the python court
  (`weft.kv.engine.attempt.v1`), and
* `--replay` the engine-shaped trace-replay receipt from the bench court
  (`weft.kv.trace_replay.v1`).

The `numbers` section is the honest labelling: everything the replay measured
locally is listed as measured, the break-even is listed as derived from that
measurement, the prefill grid is listed as hypothetical, and `engine_measured`
stays empty unless an engine actually imported. This assembler refuses to
invent an engine result: `engine_measured` is populated only from the attempts
document, and only when an import succeeded.
"""

from __future__ import annotations

import argparse
import json
import re
import sys

DOWNLOAD = re.compile(r"Downloading (\S+?)\s+\(([\d.]+)\s*([KMG])B\)")
ERROR = re.compile(r"^(ERROR:.*)$", re.MULTILINE)


def wheels(text: str) -> list[dict]:
    found = []
    for match in DOWNLOAD.finditer(text):
        size, unit = float(match.group(2)), match.group(3)
        factor = {"K": 1 / 1024, "M": 1, "G": 1024}[unit]
        found.append({"wheel": match.group(1), "mib": round(size * factor, 1)})
    return found


def first_error(text: str) -> str:
    match = ERROR.search(text)
    return match.group(1) if match else ""


def exit_text(attempt: dict) -> str:
    code = attempt.get("exit_code")
    return "timed out" if code is None else f"exit {code}"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--stamp", required=True)
    parser.add_argument("--attempts", required=True)
    parser.add_argument("--replay", required=True)
    parser.add_argument("--receipt", required=True)
    parser.add_argument("--court-json", required=True)
    parser.add_argument("--rust-rev", default="unknown")
    parser.add_argument("--go-rev", default="unknown")
    parser.add_argument("--codec-rev", default="unknown")
    parser.add_argument("--codec-go-rev", default="unknown")
    args = parser.parse_args()

    attempts = json.load(open(args.attempts, encoding="utf-8"))
    replay = json.load(open(args.replay, encoding="utf-8"))
    if replay.get("schema") != "weft.kv.trace_replay.v1":
        print("assemble_kv_engine: replay document has the wrong schema", file=sys.stderr)
        return 1
    if attempts.get("schema") != "weft.kv.engine.attempt.v1":
        print("assemble_kv_engine: attempts document has the wrong schema", file=sys.stderr)
        return 1

    raw = attempts["attempts"]
    court = json.load(open(args.court_json, encoding="utf-8"))

    vllm_full = raw["vllm_full"]
    vllm_nodeps = raw["vllm_nodeps"]
    vllm_import = raw["vllm_nodeps_import"]
    sglang_nodeps = raw["sglang_nodeps"]
    sglang_import = raw["sglang_nodeps_import"]
    sglang_full = raw["sglang_full"]

    downloaded = wheels(vllm_full.get("tail", ""))
    total_mib = round(sum(entry["mib"] for entry in downloaded), 1)
    reason = (
        f"downloaded wheels total {total_mib} MiB before unpacking "
        f"({', '.join(entry['wheel'].split('-')[0] + '-' + entry['wheel'].split('-')[1] for entry in downloaded)})"
        if downloaded
        else "pip failed before it could report the wheel set"
    )

    engine_ran = any(
        attempt.get("exit_code") == 0 for attempt in (vllm_import, sglang_import)
    )

    engine_attempt = {
        "result": attempts["result"],
        "court_limits": court,
        "vllm": {
            "command": vllm_full["command"],
            "outcome": exit_text(vllm_full),
            "error": first_error(vllm_full.get("tail", "")),
            "reason": reason,
            "tmp_free_mib_before": raw.get("tmp_free_mib_before_vllm_full"),
            "tmp_free_mib_after": raw.get("tmp_free_mib_after_vllm_full"),
            "no_deps_command": vllm_nodeps["command"],
            "no_deps_outcome": (
                f"{exit_text(vllm_nodeps)}; then "
                f"`import vllm` {exit_text(vllm_import)}"
            ),
            "no_deps_error": first_error(vllm_nodeps.get("tail", "")),
            "captured": vllm_full.get("tail", ""),
        },
        "sglang": {
            "commands": [sglang_nodeps["command"], sglang_import["command"], sglang_full["command"]],
            "no_deps_install": f"sglang --no-deps {exit_text(sglang_nodeps)}",
            "import_error": first_error(sglang_import.get("tail", ""))
            or sglang_import.get("tail", "")[-600:].strip(),
            "with_deps_error": first_error(sglang_full.get("tail", "")),
            "captured": (sglang_nodeps.get("tail", "") + "\n" + sglang_full.get("tail", ""))[-2400:],
        },
        "conclusion": (
            "no inference engine could be installed and imported inside the unchanged "
            "court limits"
            if not engine_ran
            else "an engine imported; no engine measurement was performed by this court"
        ),
        "what_would_be_needed": [
            "a court image that already contains a CPU-capable engine AND its runtime "
            "(torch, tokenizers, the model loader), so no install happens at run time",
            "or a tmpfs larger than 1 GiB for /tmp; the court's limits are not raised here",
            "a model that fits the 3 GiB memory bound alongside the engine",
        ],
        "attempts_raw": raw,
    }

    receipt = {
        "schema": "weft.kv.engine.v1",
        "stamp": args.stamp,
        "revisions": {
            "WeftFS": args.rust_rev,
            "WeftFS-Go": args.go_rev,
            "WeftCodec": args.codec_rev,
            "WeftCodec-Go": args.codec_go_rev,
        },
        "label": "inference-engine attempt and engine-shaped trace replay over real KV prefix records",
        "court": {
            "python_image": "weft/python-court:1",
            "bench_image": "weft/bench-court:1",
            "kernel": court.get("kernel", "unknown"),
            "memory": court.get("memory", "unknown"),
            "swap": court.get("swap", "unknown"),
            "pids": court.get("pids"),
            "cpus": court.get("cpus", "unknown"),
            "tmp": "1g",
        },
        "engine_attempt": engine_attempt,
        "result": (
            "blocked-no-engine; trace replay completed"
            if not engine_ran
            else "engine imported; trace replay completed"
        ),
        "fixture": replay.get("fixture"),
        "measured": replay.get("measured"),
        "decision": replay.get("decision"),
        "court_replay": replay.get("court"),
        "numbers": {
            "measured": [
                "measured.lookup_indexed",
                "measured.lookup_scan_exact_only",
                "measured.reuse_get",
                "measured.token_chain_hash_local",
                "measured.reuse_logical_bytes",
                "measured.reuse_requests",
                "measured.indexed_mismatches",
                "fixture.build_ms",
                "fixture.index_build_ms",
            ],
            "derived_from_measurement": ["decision.measured_break_even_per_token_us"],
            "hypothetical": [
                "decision.declared_prefill_fixed_ns",
                "decision.declared_prefill_ns_per_token",
                "decision.grid_per_token_us",
                "decision.rows[*].always_reuse_regret_ms",
                "decision.rows[*].always_recompute_regret_ms",
                "decision.rows[*].declared_model_regret_ms",
                "decision.rows[*].oracle_reuse_requests",
            ],
            "engine_measured": [] if not engine_ran else ["engine_attempt (see receipt)"],
            "note": (
                "No TTFT, tokens/s, hit rate or engine cache comparison exists here: no engine "
                "ran. The regret grid is a decision analysis over a hypothetical prefill cost; "
                "only the reuse costs are observed."
            ),
        },
        "non_claims": [
            "no inference engine ran; this is trace replay, and the recompute axis is hypothetical",
            "no TTFT, tokens/s, hit-rate or engine-cache claim against vLLM, SGLang or LMCache",
            "no GPU claim; the court has no /dev/nvidia*",
            "one host, synthetic trace, warm page cache",
        ],
    }
    with open(args.receipt, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(f"assemble_kv_engine: {receipt['result']} -> {args.receipt}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
