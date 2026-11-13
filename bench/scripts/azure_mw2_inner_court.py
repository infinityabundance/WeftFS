#!/usr/bin/env python3
"""Azure MW2 mixed-language court (X13/N14), inner half.

Runs inside the OOM-protected `fuse` court (the only image carrying both
toolchains). The pinned Azurite emulator runs as a sibling container started by
`scripts/azure_mw2_court.sh`, which passes its endpoint here.

Every scenario is executed and recorded even if an earlier one failed, so one
invocation captures the whole picture. Each check is counted; a scenario passes
only when every one of its checks passes. The receipt is written here; the outer
script only decides the court's exit status.

This court is evidence about a *single Azurite emulator on loopback*. Azurite is
not Azure: service-version acceptance, block limits, listing paging, replication,
media behaviour and cloud failure modes are not exercised.
"""

import hashlib
import json
import os
import signal
import subprocess
import sys
import time
from urllib.parse import urlsplit

RUST = ".docker-cache/target-rust/debug/examples/mw2_writer"
GO = "/work/.docker-cache/weft-mw2-writer"
NOW = 1700000000000000000

STAMP = os.environ["WEFT_AZURE_MW2_STAMP"]
RECEIPT = os.environ["WEFT_AZURE_MW2_RECEIPT"]
BASE_CONTAINER = os.environ.get("WEFT_AZURE_TEST_CONTAINER", "")
ENDPOINT = os.environ["WEFT_AZURE_TEST_ENDPOINT"]

parts = urlsplit(ENDPOINT)
AZURE_HOST = parts.hostname or ""
AZURE_PORT = parts.port or 10000
AZURE_PATH = parts.path or ""

results = {"scenarios": [], "failures": []}
started_epoch = time.time()


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------


def container(tag):
    name = f"weft-azm2-{STAMP.lower()}-{tag}"
    assert 3 <= len(name) <= 63, name
    assert all(c.islower() or c.isdigit() or c == "-" for c in name), name
    return name


def env_with(extra=None):
    env = os.environ.copy()
    if extra:
        env.update({k: str(v) for k, v in extra.items()})
    return env


def parse_payload(text):
    for line in reversed(text.strip().splitlines()):
        try:
            return json.loads(line)
        except Exception:
            continue
    return None


def invoke(argv, extra_env=None, timeout=300):
    proc = subprocess.run(
        argv,
        capture_output=True,
        text=True,
        env=env_with(extra_env),
        timeout=timeout,
    )
    return proc, parse_payload(proc.stdout)


def start(argv, extra_env=None, log=None):
    out = open(log, "w", encoding="utf-8") if log else subprocess.DEVNULL
    err = subprocess.STDOUT if log else subprocess.DEVNULL
    return subprocess.Popen(argv, stdout=out, stderr=err, env=env_with(extra_env))


def read_json_line(path):
    try:
        with open(path, "r", encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    return json.loads(line)
                except Exception:
                    continue
    except Exception:
        return None
    return None


LOG_DIR = "/work/.docker-cache/azm2-logs"


def log_path(name):
    os.makedirs(LOG_DIR, exist_ok=True)
    return os.path.join(LOG_DIR, name)


def wait_path(path, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if os.path.exists(path):
            return True
        time.sleep(0.02)
    return False


def wait_port(port, timeout):
    import socket

    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return True
        except OSError:
            time.sleep(0.02)
    return False


def rust(*args, container_name):
    return [
        RUST,
        *args,
        "--backend",
        "azure",
        "--container",
        container_name,
        "--fixed-timestamp-ns",
        str(NOW),
    ]


def go(*args, container_name):
    return [
        GO,
        *args,
        "--backend",
        "azure",
        "--container",
        container_name,
        "--fixed-timestamp-ns",
        str(NOW),
    ]


def new_scenario(code, label):
    scenario = {
        "code": code,
        "label": label,
        "checks": [],
        "checks_total": 0,
        "checks_passed": 0,
        "pass": False,
    }
    results["scenarios"].append(scenario)
    return scenario


def check(scenario, name, ok, detail=None):
    ok = bool(ok)
    scenario["checks"].append({"check": name, "ok": ok, "detail": detail})
    scenario["checks_total"] += 1
    if ok:
        scenario["checks_passed"] += 1
    else:
        results["failures"].append(f"{scenario['code']}: {name}: {detail}")


def finish(scenario):
    scenario["pass"] = (
        scenario["checks_total"] > 0
        and scenario["checks_passed"] == scenario["checks_total"]
    )
    return scenario["pass"]


def head_of(c, lang="rust"):
    argv = rust("head", "--store", "/tmp/weft-azm2-head", container_name=c) if lang == "rust" else go("head", "--store", "/tmp/weft-azm2-head", container_name=c)
    proc, payload = invoke(argv)
    if proc.returncode != 0 or not payload or not payload.get("ok"):
        return {"ok": False, "stderr": proc.stderr[-400:], "payload": payload}
    return payload


def verify_full(c, lang, tag):
    argv = (
        rust("verify-full", "--store", f"/tmp/weft-azm2-{tag}", container_name=c)
        if lang == "rust"
        else go("verify-full", "--store", f"/tmp/weft-azm2-{tag}", container_name=c)
    )
    proc, payload = invoke(argv)
    return proc, payload


def stat_name(c, lang, name, tag):
    argv = (
        rust("stat", "--store", f"/tmp/weft-azm2-{tag}", "--name", name, container_name=c)
        if lang == "rust"
        else go("stat", "--store", f"/tmp/weft-azm2-{tag}", "--name", name, container_name=c)
    )
    proc, payload = invoke(argv)
    return proc, payload


def expected_hash(text):
    proc, payload = invoke(
        [RUST, "content-hash", "--content", text, "--backend", "local", "--container", "x"]
    )
    if proc.returncode != 0 or not payload:
        raise RuntimeError(f"content-hash failed: {proc.stderr}")
    return payload["content_hash"]


def start_proxy(port, mode, ready=None, target="superblock", tag="proxy"):
    argv = [
        sys.executable,
        "bench/scripts/azure_mw2_proxy.py",
        "--listen",
        str(port),
        "--upstream",
        f"{AZURE_HOST}:{AZURE_PORT}",
        "--mode",
        mode,
        "--target",
        target,
    ]
    if ready:
        argv += ["--ready", ready]
    proc = subprocess.Popen(
        argv,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=open(log_path(f"{tag}.proxy.log"), "w"),
        env=env_with(),
        text=True,
    )
    if not wait_port(port, 20):
        raise RuntimeError("the court proxy never started listening")
    return proc


def stop_proxy(proc):
    try:
        proc.stdin.close()
    except Exception:
        pass
    try:
        proc.wait(timeout=10)
    except Exception:
        proc.kill()


def scenario_snapshot(scenario, c, tag):
    head = head_of(c)
    scenario["observed"] = {
        "generation": head.get("generation"),
        "head": head.get("head"),
        "superblock_sha256": head.get("superblock_sha256"),
        "commits": head.get("commits"),
        "objects": head.get("objects"),
        "manifests": head.get("manifests"),
        "indexes": head.get("indexes"),
    }
    cgroup = read_cgroup()
    scenario["cgroup"] = cgroup


def read_cgroup():
    out = {}
    for name, path in (
        ("memory_current", "/sys/fs/cgroup/memory.current"),
        ("memory_peak", "/sys/fs/cgroup/memory.peak"),
        ("pids_current", "/sys/fs/cgroup/pids.current"),
    ):
        try:
            with open(path) as handle:
                out[name] = handle.read().strip()
        except Exception:
            out[name] = None
    for name, path in (
        ("memory_events", "/sys/fs/cgroup/memory.events"),
        ("pids_events", "/sys/fs/cgroup/pids.events"),
    ):
        try:
            with open(path) as handle:
                out[name] = " ".join(handle.read().split())
        except Exception:
            out[name] = None
    try:
        with open("/sys/fs/cgroup/pids.max") as handle:
            out["pids_max"] = handle.read().strip()
    except Exception:
        out["pids_max"] = None
    return out


# ---------------------------------------------------------------------------
# S1 - disjoint mixed-language writers on one generation
# ---------------------------------------------------------------------------


def scenario_s1():
    scenario = new_scenario("S1", "disjoint mixed-language writers on one generation")
    c = container("s1")
    tag = "s1"
    store = f"/tmp/weft-azm2-{tag}"
    proc, payload = invoke(rust("init", "--store", store, container_name=c))
    check(scenario, "init", proc.returncode == 0 and payload and payload.get("ok"), proc.stderr[-300:])

    writers = 4
    count = 2
    rust_writers = [f"r{i}" for i in range(writers)]
    go_writers = [f"g{i}" for i in range(writers)]
    procs = {}
    reports = {}
    for writer in rust_writers:
        procs[writer] = start(
            rust(
                "write",
                "--store",
                store,
                "--writer",
                writer,
                "--prefix",
                writer,
                "--count",
                str(count),
                container_name=c,
            ),
            log=log_path(f"{tag}-{writer}.json"),
        )
    for writer in go_writers:
        procs[writer] = start(
            go(
                "write",
                "--store",
                store,
                "--writer",
                writer,
                "--prefix",
                writer,
                "--count",
                str(count),
                container_name=c,
            ),
            log=log_path(f"{tag}-{writer}.json"),
        )
    for writer, proc in procs.items():
        proc.wait(timeout=600)
        report = read_json_line(log_path(f"{tag}-{writer}.json"))
        reports[writer] = report
        check(
            scenario,
            f"writer {writer} committed {count}",
            report is not None
            and report.get("committed") == count
            and report.get("conflicts") == 0
            and not report.get("errors"),
            report,
        )

    head_rust = head_of(c)
    head_go = head_of(c, lang="go")
    check(
        scenario,
        "both languages report the same head",
        head_rust.get("head") == head_go.get("head")
        and head_rust.get("generation") == head_go.get("generation"),
        {"rust": head_rust, "go": head_go},
    )
    expected_generation = 1 + writers * count * 2
    # A concurrent open may observe another writer's not-yet-published commit
    # record and skip that generation number; the store never reuses one. The
    # head therefore cannot be *below* genesis plus the commits, and the exact
    # state is proved by verify --full rather than by arithmetic on gaps.
    check(
        scenario,
        "the head generation accounts for every committed transaction",
        (head_rust.get("generation") or 0) >= expected_generation,
        {"generation": head_rust.get("generation"), "minimum": expected_generation},
    )
    observed_heads = [
        report.get("head")
        for report in reports.values()
        if report and report.get("head")
    ]
    check(
        scenario,
        "the final head is one of the writers' observed heads",
        head_rust.get("head") in observed_heads,
        {"head": head_rust.get("head"), "observed": sorted(set(observed_heads))},
    )

    for writer in rust_writers:
        proc, payload = invoke(
            rust("verify", "--store", store, "--writer", writer, "--prefix", writer, "--count", str(count), container_name=c)
        )
        check(scenario, f"rust reads {writer}", payload is not None and payload.get("ok"), payload)
    for writer in go_writers:
        proc, payload = invoke(
            go("verify", "--store", store, "--writer", writer, "--prefix", writer, "--count", str(count), container_name=c)
        )
        check(scenario, f"go reads {writer}", payload is not None and payload.get("ok"), payload)
    for writer in go_writers:
        proc, payload = invoke(
            rust("verify", "--store", store, "--writer", writer, "--prefix", writer, "--count", str(count), container_name=c)
        )
        check(scenario, f"rust cross-reads {writer}", payload is not None and payload.get("ok"), payload)
    for writer in rust_writers:
        proc, payload = invoke(
            go("verify", "--store", store, "--writer", writer, "--prefix", writer, "--count", str(count), container_name=c)
        )
        check(scenario, f"go cross-reads {writer}", payload is not None and payload.get("ok"), payload)

    for lang in ("rust", "go"):
        proc, payload = verify_full(c, lang, tag)
        check(
            scenario,
            f"{lang} verify --full",
            proc.returncode == 0 and payload is not None and payload.get("ok"),
            payload if not payload or not payload.get("ok") else None,
        )
    scenario_snapshot(scenario, c, tag)
    return finish(scenario)


# ---------------------------------------------------------------------------
# S2 - conflicting mixed-language writers, released simultaneously
# ---------------------------------------------------------------------------


def scenario_s2():
    scenario = new_scenario("S2", "conflicting mixed-language writers on one name")
    c = container("s2")
    tag = "s2"
    store = f"/tmp/weft-azm2-{tag}"
    proc, payload = invoke(rust("init", "--store", store, container_name=c))
    check(scenario, "init", proc.returncode == 0 and payload and payload.get("ok"), proc.stderr[-300:])

    rounds = 6
    winners = 0
    rounds_recorded = []
    for index in range(rounds):
        name = f"hot-{index:02d}"
        before = head_of(c)
        rust_tx = hashlib.sha256(f"azm2-s2-rust-{index}".encode()).hexdigest()[:32]
        go_tx = hashlib.sha256(f"azm2-s2-go-{index}".encode()).hexdigest()[:32]
        rust_proc = start(
            rust(
                "write", "--store", store, "--writer", f"r{index}", "--count", "1",
                "--hot", name, "--tx-id", rust_tx, container_name=c,
            ),
            log=log_path(f"{tag}-r{index}.json"),
        )
        go_proc = start(
            go(
                "write", "--store", store, "--writer", f"g{index}", "--count", "1",
                "--hot", name, "--tx-id", go_tx, container_name=c,
            ),
            log=log_path(f"{tag}-g{index}.json"),
        )
        rust_proc.wait(timeout=300)
        go_proc.wait(timeout=300)
        rust_report = read_json_line(log_path(f"{tag}-r{index}.json"))
        go_report = read_json_line(log_path(f"{tag}-g{index}.json"))
        rust_committed = (rust_report or {}).get("committed", 0)
        go_committed = (go_report or {}).get("committed", 0)
        rust_conflicts = (rust_report or {}).get("conflicts", 0)
        go_conflicts = (go_report or {}).get("conflicts", 0)
        one_winner = rust_committed + go_committed == 1
        check(
            scenario,
            f"round {index}: exactly one writer committed",
            one_winner,
            {"rust": rust_report, "go": go_report},
        )
        loser_conflicted = (
            (rust_committed == 0 and rust_conflicts == 1 and not (rust_report or {}).get("errors"))
            or (go_committed == 0 and go_conflicts == 1 and not (go_report or {}).get("errors"))
        )
        check(scenario, f"round {index}: the loser reported a typed Conflict", loser_conflicted,
              {"rust": rust_report, "go": go_report})
        winner = "rust" if rust_committed == 1 else "go"
        winner_payload = f"{'r' if winner == 'rust' else 'g'}{index}:0"
        expected = expected_hash(winner_payload)
        restat, report = stat_name(c, "rust", name, tag)
        present = bool(report and report.get("present"))
        check(
            scenario,
            f"round {index}: the winner's bytes are exactly present",
            present and report.get("content_hash") == expected,
            {"winner": winner, "stat": report},
        )
        after = head_of(c)
        advice = (after.get("generation") or 0) - (before.get("generation") or 0)
        check(
            scenario,
            f"round {index}: the head advanced by exactly one generation",
            advice == 1,
            {"before": before.get("generation"), "after": after.get("generation")},
        )
        winners += 1
        rounds_recorded.append({
            "round": index,
            "winner": winner,
            "generation_before": before.get("generation"),
            "generation_after": after.get("generation"),
        })

    scenario["rounds"] = rounds_recorded
    head = head_of(c)
    check(
        scenario,
        "generation equals genesis plus every winner",
        head.get("generation") == 1 + winners,
        {"generation": head.get("generation"), "expected": 1 + winners},
    )
    for lang in ("rust", "go"):
        proc, payload = verify_full(c, lang, tag)
        check(
            scenario,
            f"{lang} verify --full",
            proc.returncode == 0 and payload is not None and payload.get("ok"),
            payload if not payload or not payload.get("ok") else None,
        )
    scenario_snapshot(scenario, c, tag)
    return finish(scenario)


# ---------------------------------------------------------------------------
# S3 - ambiguous success on the conditional head write, both directions
# ---------------------------------------------------------------------------


def ambiguous_round(scenario, label, killed_lang, resolver_lang, tag, port):
    c = container(tag)
    store = f"/tmp/weft-azm2-{tag}"
    proc, payload = invoke(rust("init", "--store", store, container_name=c))
    check(scenario, f"{label}: init", proc.returncode == 0 and payload and payload.get("ok"), proc.stderr[-300:])
    base = head_of(c)
    ready = f"/tmp/weft-azm2-{tag}.ready"
    if os.path.exists(ready):
        os.remove(ready)
    proxy = start_proxy(port, "swallow", ready=ready, tag=tag)
    tx_id = hashlib.sha256(f"azm2-s3-{tag}".encode()).hexdigest()[:32]
    writer_args = [
        "write", "--store", store, "--writer", f"k{tag}", "--count", "1",
        "--prefix", f"k{tag}", "--tx-id", tx_id,
    ]
    kill_env = {
        "WEFT_AZURE_AMBIGUOUS_HEAD": "1",
        "WEFT_AZURE_AMBIGUOUS_READY": ready,
        "WEFT_AZURE_TEST_ENDPOINT": f"http://127.0.0.1:{port}{AZURE_PATH}",
        "WEFT_AZURE_ENDPOINT": f"http://127.0.0.1:{port}{AZURE_PATH}",
        "WEFT_AZURE_TEST_CONTAINER": c,
    }
    argv = rust(*writer_args, container_name=c) if killed_lang == "rust" else go(*writer_args, container_name=c)
    writer = start(argv, extra_env=kill_env, log=log_path(f"{tag}-killed.log"))
    try:
        code = writer.wait(timeout=180)
    finally:
        stop_proxy(proxy)
    marker = read_json_line(ready)
    check(scenario, f"{label}: the fault point fired (marker written)", marker is not None, {"ready": ready})
    check(
        scenario,
        f"{label}: the conditional head write was applied upstream",
        marker is not None and marker.get("status") in (200, 201),
        marker,
    )
    check(
        scenario,
        f"{label}: the writer died by SIGKILL before reading the response",
        code == -signal.SIGKILL,
        {"exit": code},
    )
    resolver = go if resolver_lang == "go" else rust
    lookup_proc, lookup = invoke(resolver("outcome", "--store", store, "--tx-id", tx_id, container_name=c))
    check(
        scenario,
        f"{label}: the outcome is unknown until it is resolved",
        lookup is not None and lookup.get("state") == "unknown",
        lookup,
    )
    before_resolve = head_of(c)
    resolve_proc, resolved = invoke(
        resolver(*writer_args, container_name=c), timeout=300
    )
    check(
        scenario,
        f"{label}: the resolver reports the transaction committed",
        resolved is not None and resolved.get("committed") == 1 and not resolved.get("errors"),
        resolved,
    )
    check(
        scenario,
        f"{label}: resolution did not publish a second generation",
        resolved is not None and resolved.get("generation") == before_resolve.get("generation"),
        {"resolved": resolved, "head": before_resolve},
    )
    after = head_of(c)
    check(
        scenario,
        f"{label}: the head is unchanged by resolution",
        after.get("generation") == before_resolve.get("generation")
        and after.get("superblock_sha256") == before_resolve.get("superblock_sha256"),
        {"before": before_resolve, "after": after},
    )
    expected = expected_hash(f"k{tag}:0")
    _, report = stat_name(c, "rust", f"k{tag}-000000", tag)
    check(
        scenario,
        f"{label}: the killed transaction's bytes are exactly present",
        report is not None and report.get("present") and report.get("content_hash") == expected,
        report,
    )
    for lang in ("rust", "go"):
        proc, payload = verify_full(c, lang, tag)
        check(
            scenario,
            f"{label}: {lang} verify --full",
            proc.returncode == 0 and payload is not None and payload.get("ok"),
            payload if not payload or not payload.get("ok") else None,
        )
    return {"base_generation": base.get("generation"), "marker": marker}


def scenario_s3():
    scenario = new_scenario(
        "S3", "ambiguous success on the conditional head write, Rust-killed and Go-killed"
    )
    details = {}
    details["rust_killed_go_resolved"] = ambiguous_round(
        scenario, "rust-killed", "rust", "go", "s3a", 10101
    )
    details["go_killed_rust_resolved"] = ambiguous_round(
        scenario, "go-killed", "go", "rust", "s3b", 10102
    )
    scenario["rounds"] = details
    scenario_snapshot(scenario, container("s3b"), "s3b")
    return finish(scenario)


# ---------------------------------------------------------------------------
# S4 - fenced takeover with a suspended stale writer
# ---------------------------------------------------------------------------


def fence_round(scenario, label, stale_lang, successor_lang, tag):
    c = container(tag)
    store = f"/tmp/weft-azm2-{tag}"
    proc, payload = invoke(rust("init", "--store", store, container_name=c))
    check(scenario, f"{label}: init", proc.returncode == 0 and payload and payload.get("ok"), proc.stderr[-300:])
    ready = f"/tmp/weft-azm2-{tag}.ready"
    gate = f"/tmp/weft-azm2-{tag}.fifo"
    for path in (ready, gate):
        if os.path.exists(path):
            os.remove(path)
    lease = {"WEFT_AZURE_LOCK_LEASE_SECS": "3"}
    pause_env = dict(lease)
    pause_env["WEFT_FENCE_PAUSE_AFTER_FENCE"] = "1"
    pause_env["WEFT_FENCE_AFTER_FENCE_READY"] = ready
    if stale_lang == "go":
        os.mkfifo(gate)
        pause_env["WEFT_FENCE_AFTER_FENCE_GATE"] = gate
    stale_args = [
        "single", "--store", store, "--name", "stale.bin", "--content", "stale", "--seed", "stale-1",
    ]
    stale_argv = rust(*stale_args, container_name=c) if stale_lang == "rust" else go(*stale_args, container_name=c)
    stale = start(stale_argv, extra_env=pause_env, log=log_path(f"{tag}-stale.log"))
    check(scenario, f"{label}: the stale writer reached the fence", wait_path(ready, 120), ready)

    successor = rust if successor_lang == "rust" else go
    successor_args = [
        "single", "--store", store, "--name", "winner.bin", "--content", "winner", "--seed", "winner-1",
    ]
    took_over = False
    attempts = 0
    for _ in range(30):
        attempts += 1
        proc, payload = invoke(successor(*successor_args, container_name=c), extra_env=lease, timeout=180)
        if proc.returncode == 0 and payload and payload.get("ok"):
            took_over = True
            break
        time.sleep(0.5)
    check(scenario, f"{label}: a successor took over the expired lease", took_over,
          {"attempts": attempts, "last": payload})

    head_before = head_of(c)
    if stale_lang == "rust":
        os.kill(stale.pid, signal.SIGCONT)
    else:
        with open(gate, "wb") as handle:
            handle.write(b"x")
    stale.wait(timeout=180)
    stale_report = read_json_line(log_path(f"{tag}-stale.log"))
    check(
        scenario,
        f"{label}: the resumed stale writer was refused Busy",
        stale_report is not None
        and stale_report.get("ok") is False
        and stale_report.get("class") == "Busy",
        stale_report,
    )
    head_after = head_of(c)
    check(
        scenario,
        f"{label}: the refused stale writer changed nothing",
        head_before.get("generation") == head_after.get("generation")
        and head_before.get("superblock_sha256") == head_after.get("superblock_sha256"),
        {"before": head_before, "after": head_after},
    )
    _, stale_stat = stat_name(c, "rust", "stale.bin", tag)
    check(
        scenario,
        f"{label}: the stale writer published no file",
        stale_stat is not None and stale_stat.get("present") is False,
        stale_stat,
    )
    expected = expected_hash("winner")
    _, winner_stat = stat_name(c, "rust", "winner.bin", tag)
    check(
        scenario,
        f"{label}: the successor's file is exactly present",
        winner_stat is not None
        and winner_stat.get("present")
        and winner_stat.get("content_hash") == expected,
        winner_stat,
    )
    for lang in ("rust", "go"):
        proc, payload = verify_full(c, lang, tag)
        check(
            scenario,
            f"{label}: {lang} verify --full",
            proc.returncode == 0 and payload is not None and payload.get("ok"),
            payload if not payload or not payload.get("ok") else None,
        )
    return {"successor": successor_lang, "attempts": attempts}


def scenario_s4():
    scenario = new_scenario("S4", "fenced takeover with a suspended stale writer")
    rounds = {}
    rounds["rust_stale_go_successor"] = fence_round(scenario, "rust-stale", "rust", "go", "s4a")
    rounds["go_stale_rust_successor"] = fence_round(scenario, "go-stale", "go", "rust", "s4b")
    scenario["rounds"] = rounds
    scenario["lease_secs"] = 3
    scenario_snapshot(scenario, container("s4b"), "s4b")
    return finish(scenario)


# ---------------------------------------------------------------------------
# S5 - stale reader never observes newer data
# ---------------------------------------------------------------------------


def stale_reader_round(scenario, label, lang, tag):
    c = container(tag)
    store = f"/tmp/weft-azm2-{tag}"
    proc, payload = invoke(rust("init", "--store", store, container_name=c))
    check(scenario, f"{label}: init", proc.returncode == 0 and payload and payload.get("ok"), proc.stderr[-300:])
    proc, payload = invoke(
        rust("write", "--store", store, "--writer", "base", "--count", "1", "--prefix", "base", container_name=c)
    )
    check(scenario, f"{label}: baseline commit", payload is not None and payload.get("committed") == 1, payload)
    ready = f"/tmp/weft-azm2-{tag}.ready"
    gate = f"/tmp/weft-azm2-{tag}.gate"
    for path in (ready, gate):
        if os.path.exists(path):
            os.remove(path)
    argv = rust("hold-read", "--store", store, "--name", "new.bin", "--ready", ready, "--gate", gate, container_name=c) if lang == "rust" else go("hold-read", "--store", store, "--name", "new.bin", "--ready", ready, "--gate", gate, container_name=c)
    reader = start(argv, log=log_path(f"{tag}-reader.log"))
    check(scenario, f"{label}: the reader announced its held generation", wait_path(ready, 120), ready)
    info = read_json_line(ready) or {}
    held_generation = info.get("generation")
    # Publish newer data while the handle is held.
    proc, payload = invoke(
        go("write", "--store", store, "--writer", "later", "--count", "1", "--prefix", "new", container_name=c)
    )
    check(scenario, f"{label}: a newer commit was published", payload is not None and payload.get("committed") == 1, payload)
    with open(gate, "w", encoding="utf-8") as handle:
        handle.write("go\n")
    reader.wait(timeout=120)
    report = read_json_line(log_path(f"{tag}-reader.log")) or {}
    check(
        scenario,
        f"{label}: the held handle still reports its original generation",
        report.get("generation") == held_generation,
        {"held": held_generation, "report": report},
    )
    check(
        scenario,
        f"{label}: the held handle never observes the newer object",
        (report.get("after") or {}).get("present") is False,
        report.get("after"),
    )
    for verifier in ("rust", "go"):
        _, fresh = stat_name(c, verifier, "new-000000", tag)
        check(
            scenario,
            f"{label}: a fresh {verifier} open does observe it",
            fresh is not None and fresh.get("present") is True,
            fresh,
        )
    expected = expected_hash("later:0")
    _, fresh_rust = stat_name(c, "rust", "new-000000", tag)
    check(
        scenario,
        f"{label}: the fresh read is the newer bytes",
        fresh_rust is not None and fresh_rust.get("content_hash") == expected,
        fresh_rust,
    )
    return {"held_generation": held_generation, "after_generation": report.get("generation")}


def scenario_s5():
    scenario = new_scenario("S5", "a held reader never observes a newer generation")
    rounds = {}
    rounds["rust_reader"] = stale_reader_round(scenario, "rust-reader", "rust", "s5a")
    rounds["go_reader"] = stale_reader_round(scenario, "go-reader", "go", "s5b")
    scenario["rounds"] = rounds
    scenario_snapshot(scenario, container("s5b"), "s5b")
    return finish(scenario)


# ---------------------------------------------------------------------------
# S6 - concurrent GC while mixed writers publish
# ---------------------------------------------------------------------------


def scenario_s6():
    scenario = new_scenario("S6", "concurrent GC while mixed writers publish")
    c = container("s6")
    tag = "s6"
    store = f"/tmp/weft-azm2-{tag}"
    proc, payload = invoke(rust("init", "--store", store, container_name=c))
    check(scenario, "init", proc.returncode == 0 and payload and payload.get("ok"), proc.stderr[-300:])
    # Establish the optimistic policy and a retained snapshot root.
    for step in (
        ("write", "--writer", "old", "--count", "1", "--prefix", "garbage"),
        ("snapshot", "--name", "keep"),
        ("write", "--writer", "keep", "--count", "1", "--prefix", "keep"),
        ("write", "--writer", "trash", "--count", "1", "--prefix", "trash"),
    ):
        proc, payload = invoke(rust(*step, "--store", store, container_name=c))
        check(scenario, f"setup {step[0]} {step[1]}", payload is not None and payload.get("ok"),
              payload if not payload or not payload.get("ok") else None)
    proc, payload = invoke(rust("delete", "--store", store, "--name", "trash-000000", container_name=c))
    check(scenario, "unreachable data became garbage", payload is not None and payload.get("ok"), payload)
    # The sweep's age gate is the contract that makes a concurrent pass safe: a
    # blob younger than the grace window is presumed live. The court therefore
    # ages the garbage past the window before the writers start, and uses a
    # grace longer than any transaction in this workload. With a grace shorter
    # than a writer's prepare-to-publish interval the same experiment loses live
    # data (retained receipt `azure-mw2-*` from the 1 s configuration in
    # `bench/receipts/`); that is a configuration outside the contract, not a
    # pass.
    grace_secs = int(os.environ.get("WEFT_AZURE_MW2_GC_GRACE", "10"))
    time.sleep(grace_secs + 1.5)

    writers = []
    counts = 3
    for writer in ("r0", "r1"):
        writers.append((writer, start(
            rust("write", "--store", store, "--writer", writer, "--count", str(counts), "--prefix", writer, container_name=c),
            log=log_path(f"{tag}-{writer}.json"),
        )))
    for writer in ("g0", "g1"):
        writers.append((writer, start(
            go("write", "--store", store, "--writer", writer, "--count", str(counts), "--prefix", writer, container_name=c),
            log=log_path(f"{tag}-{writer}.json"),
        )))
    gc_passes = []
    for _ in range(3):
        proc, payload = invoke(
            rust("gc", "--apply", "--store", store, container_name=c),
            extra_env={"WEFT_MW2_GC_GRACE_SECS": str(grace_secs)},
        )
        gc_passes.append(payload if payload else {"ok": False, "stderr": proc.stderr[-200:]})
        if payload is None or not payload.get("ok"):
            break
    for writer, proc in writers:
        proc.wait(timeout=600)
        report = read_json_line(log_path(f"{tag}-{writer}.json"))
        check(
            scenario,
            f"writer {writer} published without a GC-caused failure",
            report is not None
            and report.get("committed") == counts
            and not report.get("errors"),
            report,
        )
    scenario["gc_passes"] = gc_passes
    scenario["grace_secs"] = grace_secs
    check(
        scenario,
        "every GC pass succeeded",
        all(p.get("ok") for p in gc_passes) and len(gc_passes) == 3,
        gc_passes,
    )
    deleted = sum(int(p.get("deleted_bytes") or 0) for p in gc_passes)
    check(scenario, "GC reclaimed the unreachable data", deleted > 0, {"deleted_bytes": deleted})
    for lang in ("rust", "go"):
        proc, payload = verify_full(c, lang, tag)
        check(
            scenario,
            f"{lang} verify --full after concurrent GC",
            proc.returncode == 0 and payload is not None and payload.get("ok"),
            payload if not payload or not payload.get("ok") else None,
        )
    # The snapshot root must have kept the data garbage-* no longer reaches.
    expected = expected_hash("old:0")
    for lang in ("rust", "go"):
        proc, payload = invoke(
            (rust if lang == "rust" else go)(
                "read-snapshot", "--store", store, "--snapshot", "keep", "--name", "garbage-000000", container_name=c
            )
        )
        check(
            scenario,
            f"{lang} reads the snapshot-protected object after GC",
            payload is not None
            and payload.get("ok")
            and (payload.get("read") or {}).get("content_hash") == expected,
            payload,
        )
    for writer in ("r0", "r1", "g0", "g1"):
        for lang in ("rust", "go"):
            proc, payload = invoke(
                (rust if lang == "rust" else go)(
                    "verify", "--store", store, "--writer", writer, "--prefix", writer,
                    "--count", str(counts), container_name=c,
                )
            )
            check(
                scenario,
                f"{lang} reads writer {writer} after GC",
                payload is not None and payload.get("ok"),
                payload,
            )
    scenario_snapshot(scenario, c, tag)
    return finish(scenario)


# ---------------------------------------------------------------------------
# S7 - fail-closed mode detection against an endpoint that accepts stale writes
# ---------------------------------------------------------------------------


def open_check(c, lang, mode, tag):
    argv = (
        rust("open-check", "--store", f"/tmp/weft-azm2-{tag}", "--mode", mode, container_name=c)
        if lang == "rust"
        else go("open-check", "--store", f"/tmp/weft-azm2-{tag}", "--mode", mode, container_name=c)
    )
    proc, payload = invoke(argv)
    return proc, payload


def scenario_s7():
    scenario = new_scenario("S7", "fail-closed mode detection against a permissive endpoint")
    cases = []
    for mode, port, proxy_mode, tag in (
        ("single", 10201, "ignores-if-match", "s7a"),
        ("single", 10202, "ignores-if-none-match", "s7b"),
        ("optimistic", 10203, "ignores-if-match", "s7c"),
        ("optimistic", 10204, "ignores-if-none-match", "s7d"),
    ):
        c = container(tag)
        store = f"/tmp/weft-azm2-{tag}"
        proc, payload = invoke(rust("init", "--store", store, container_name=c))
        check(scenario, f"{tag}: init", proc.returncode == 0 and payload and payload.get("ok"), proc.stderr[-300:])
        if mode == "optimistic":
            proc, payload = invoke(
                rust("write", "--store", store, "--writer", "p", "--count", "1", "--prefix", "p", container_name=c)
            )
            check(scenario, f"{tag}: optimistic policy established",
                  payload is not None and payload.get("committed") == 1, payload)
        # Positive control on the real endpoint.
        proc, payload = open_check(c, "rust", mode, tag)
        check(
            scenario,
            f"{tag}: control open against the real endpoint succeeds",
            payload is not None and (payload.get("open") or {}).get("ok") is True,
            payload,
        )
        proxy = start_proxy(port, proxy_mode, tag=tag)
        permissive = {
            "WEFT_AZURE_TEST_ENDPOINT": f"http://127.0.0.1:{port}{AZURE_PATH}",
            "WEFT_AZURE_ENDPOINT": f"http://127.0.0.1:{port}{AZURE_PATH}",
            "WEFT_AZURE_TEST_CONTAINER": c,
        }
        for lang in ("rust", "go"):
            argv = (
                rust("open-check", "--store", store, "--mode", mode, container_name=c)
                if lang == "rust"
                else go("open-check", "--store", store, "--mode", mode, container_name=c)
            )
            proc, payload = invoke(argv, extra_env=permissive)
            opened = (payload or {}).get("open") or {}
            check(
                scenario,
                f"{tag}: {lang} writable open refused the {proxy_mode} endpoint",
                proc.returncode == 0
                and payload is not None
                and opened.get("ok") is False
                and opened.get("class") in ("UnsupportedVersion", "Unsupported"),
                {"payload": payload, "stderr": proc.stderr[-300:]},
            )
        stop_proxy(proxy)
        cases.append({"tag": tag, "mode": mode, "proxy": proxy_mode})
    scenario["cases"] = cases
    return finish(scenario)


# ---------------------------------------------------------------------------
# S8 - cross-language verification and deterministic parity
# ---------------------------------------------------------------------------


def scenario_s8():
    scenario = new_scenario("S8", "cross-language verification and deterministic parity")
    # Deterministic parity: the same transaction in both languages, on two
    # separate containers, must produce identical manifest and head bytes.
    parity = {}
    parity_containers = {}
    for lang in ("rust", "go"):
        c = container(f"s8p{lang[0]}")
        parity_containers[lang] = c
        store = f"/tmp/weft-azm2-s8p{lang[0]}"
        invoke(rust("init", "--store", store, container_name=c))
        argv = (
            rust("parity", "--store", store, "--name", "parity.bin", "--content", "parity-body",
                 "--seed", "parity-seed", container_name=c)
            if lang == "rust"
            else go("parity", "--store", store, "--name", "parity.bin", "--content", "parity-body",
                    "--seed", "parity-seed", container_name=c)
        )
        proc, payload = invoke(argv)
        parity[lang] = payload
        check(scenario, f"{lang} parity transaction committed", payload is not None and payload.get("ok"), payload)
    fields = ("head", "manifest", "content_root", "object_id", "entry_id", "version", "size")
    differing = {
        field: [parity["rust"].get(field), parity["go"].get(field)]
        for field in fields
        if (parity["rust"] or {}).get(field) != (parity["go"] or {}).get(field)
    }
    check(scenario, "Rust and Go produce byte-identical parity records", not differing, differing)

    # Cross-language verification of one store both languages wrote.
    c = container("s8m")
    store = f"/tmp/weft-azm2-s8m"
    invoke(rust("init", "--store", store, container_name=c))
    for writer, lang in (("r0", "rust"), ("g0", "go")):
        argv = (
            rust("write", "--store", store, "--writer", writer, "--count", "2", "--prefix", writer, container_name=c)
            if lang == "rust"
            else go("write", "--store", store, "--writer", writer, "--count", "2", "--prefix", writer, container_name=c)
        )
        proc, payload = invoke(argv)
        check(scenario, f"{lang} wrote {writer}", payload is not None and payload.get("committed") == 2, payload)
    for lang in ("rust", "go"):
        proc, payload = verify_full(c, lang, "s8m")
        check(
            scenario,
            f"{lang} verify --full over the mixed store",
            proc.returncode == 0 and payload is not None and payload.get("ok"),
            payload if not payload or not payload.get("ok") else None,
        )
    # Each language reads the other's parity store too.
    for lang, other in (("rust", "go"), ("go", "rust")):
        proc, payload = invoke(
            (rust if lang == "rust" else go)(
                "read", "--store", f"/tmp/weft-azm2-s8p{other[0]}", "--name", "parity.bin",
                container_name=parity_containers[other],
            )
        )
        check(
            scenario,
            f"{lang} reads the store {other} wrote",
            payload is not None
            and payload.get("ok")
            and payload.get("content_hash") == (parity["rust"] or {}).get("content_root"),
            payload,
        )
    scenario["parity"] = parity
    return finish(scenario)


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------


def main():
    scenarios_env = os.environ.get("WEFT_AZURE_MW2_SCENARIOS", "S1,S2,S3,S4,S5,S6,S7,S8")
    wanted = {part.strip().upper() for part in scenarios_env.split(",") if part.strip()}
    dispatch = {
        "S1": scenario_s1,
        "S2": scenario_s2,
        "S3": scenario_s3,
        "S4": scenario_s4,
        "S5": scenario_s5,
        "S6": scenario_s6,
        "S7": scenario_s7,
        "S8": scenario_s8,
    }
    print(
        json.dumps(
            {
                "court": "azure_mw2",
                "stamp": STAMP,
                "endpoint": ENDPOINT,
                "scenarios": sorted(wanted),
                "kernel": os.uname().release,
                "limits": {
                    "memory": os.environ.get("WEFT_COURT_MEMORY"),
                    "cpus": os.environ.get("WEFT_COURT_CPUS"),
                    "pids": os.environ.get("WEFT_COURT_PIDS"),
                },
            },
            sort_keys=True,
        ),
        flush=True,
    )
    for code in ("S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8"):
        if code not in wanted:
            continue
        print(f"== {code} ==", flush=True)
        try:
            ok = dispatch[code]()
        except Exception as exc:  # a crashed scenario is a failed scenario
            results["failures"].append(f"{code}: uncaught {type(exc).__name__}: {exc}")
            ok = False
        print(f"== {code}: {'pass' if ok else 'FAIL'} ==", flush=True)

    finished_epoch = time.time()
    cgroup = read_cgroup()
    total_checks = sum(s["checks_total"] for s in results["scenarios"])
    passed_checks = sum(s["checks_passed"] for s in results["scenarios"])
    receipt = {
        "schema": "weft-azure-mw2-court-v1",
        "stamp": STAMP,
        "generated_utc": STAMP,
        "result": "pass" if not results["failures"] else "fail",
        "label": (
            "Single-host Azurite Blob emulator on loopback inside the OOM-protected Docker "
            "court. Azurite is not Azure: no cloud, node, fleet, media, power-loss, or "
            "replication claim. Process-fault proxies only; no physical fault."
        ),
        "azurite": {
            "product": "Azurite Blob service (mcr.microsoft.com/azure-storage/azurite)",
            "release": os.environ.get("WEFT_AZURE_VERSION"),
            "image": os.environ.get("WEFT_AZURE_IMAGE"),
            "image_id": os.environ.get("WEFT_AZURE_IMAGE_ID"),
            "image_digest": os.environ.get("WEFT_AZURE_SERVER_DIGEST"),
            "data_dir": "tmpfs /data, memory-backed",
            "api_version": "2021-12-02",
            "auth": "SharedKey with the published Azurite development account key",
            "distinct_from_azure": (
                "Azurite is Microsoft's emulator. Service-version acceptance, block-blob "
                "limits, listing paging, replication, media and cloud failure modes are not "
                "exercised."
            ),
        },
        "revisions": {
            "weftfs": os.environ.get("WEFT_REPO_WEFTFS"),
            "weftfs_go": os.environ.get("WEFT_REPO_WEFTFS_GO"),
            "weftcodec": os.environ.get("WEFT_REPO_WEFTCODEC"),
            "weftcodec_go": os.environ.get("WEFT_REPO_WEFTCODEC_GO"),
        },
        "court": {
            "image": os.environ.get("WEFT_COURT_IMAGE"),
            "image_id": os.environ.get("WEFT_COURT_IMAGE_ID"),
            "kind": "fuse (the dual-toolchain court; FUSE itself is not used)",
            "kernel": os.uname().release,
            "limits": {
                "memory": os.environ.get("WEFT_COURT_MEMORY"),
                "memory_swap": os.environ.get("WEFT_COURT_MEMORY"),
                "cpus": os.environ.get("WEFT_COURT_CPUS"),
                "pids": os.environ.get("WEFT_COURT_PIDS"),
                "nproc": os.environ.get("WEFT_COURT_NPROC"),
                "tmpfs": "/tmp 1g",
            },
            "endpoint": ENDPOINT,
            "base_container": BASE_CONTAINER,
            "accounts": "one shared Azurite development account",
        },
        "fault_points": [
            {
                "name": "WEFT_AZURE_AMBIGUOUS_HEAD / WEFT_AZURE_AMBIGUOUS_READY",
                "where": (
                    "court driver (Rust examples/mw2_writer.rs, Go cmd/mw2writer): a watchdog "
                    "SIGKILLs the writer when the harness proxy marks that the conditional head "
                    "PUT has been applied upstream and its response is withheld"
                ),
                "inert_when": "the environment variable is unset",
            },
            {
                "name": "WEFT_FENCE_PAUSE_AFTER_FENCE / WEFT_FENCE_AFTER_FENCE_READY / _GATE",
                "where": "shipped fencing hook (src/fence_pause.rs, WeftFS-Go/store/fence_pause.go)",
                "inert_when": "the environment variable is unset",
            },
            {
                "name": "WEFT_AZURE_LOCK_LEASE_SECS",
                "where": "Azure writer lease; the court shortens it to 3 s so a takeover is reachable",
                "inert_when": "unset (default 900 s in the library, 60 s in the court driver)",
            },
            {
                "name": "WEFT_MW2_GC_GRACE_SECS",
                "where": "court driver only; shortens the sweep age gate so unreachable data is reclaimable",
                "inert_when": "unset (the library default stays 3600 s)",
            },
        ],
        "scenarios": results["scenarios"],
        "summary": {
            "scenarios": len(results["scenarios"]),
            "scenarios_passed": sum(1 for s in results["scenarios"] if s["pass"]),
            "checks": total_checks,
            "checks_passed": passed_checks,
            "failures": len(results["failures"]),
        },
        "failures": results["failures"],
        "cgroup": cgroup,
        "duration_seconds": round(finished_epoch - started_epoch, 3),
    }
    os.makedirs("bench/receipts", exist_ok=True)
    with open(RECEIPT, "w", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=1, sort_keys=True)
        handle.write("\n")
    print(json.dumps(receipt["summary"], sort_keys=True), flush=True)
    print(f"azure_mw2_inner_court: receipt {RECEIPT}", flush=True)
    return 0 if receipt["result"] == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
