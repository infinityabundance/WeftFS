"""Bounded checkpoint queue tests (R12c item 4).

What these prove, in the Docker python court with the real Rust CLI:

* a refused save is a **typed error** that publishes nothing and is counted;
* an admitted save that is cancelled before it starts publishes nothing;
* an admitted save interrupted at each existing publication phase leaves
  old-or-new-whole state and never a partial checkpoint;
* two sessions on one store keep the existing writer fencing (the second is
  refused, not silently interleaved);
* drain accounts for every admitted save (completed or explicitly failed);
* the queue bound is enforced at *bound + 1*;
* no silent skips: requested == completed + failed + cancelled, and saves land
  in submission order.

The crash and kill tests are process faults, not media power loss.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

from weftcodec.dcp import (
    DEFAULT_CLI,
    CheckpointSession,
    DcpError,
    QueueClosed,
    QueueFull,
    SaveNotStarted,
    _estimate_state_bytes,
    init_store,
    verify_checkpoint,
)

HAS_TORCH = __import__("importlib").util.find_spec("torch") is not None
WEFTFS_CLI = Path(os.environ.get("WEFTFS_CLI", DEFAULT_CLI))
PYTHON_ROOT = str(Path(__file__).resolve().parents[1])

pytestmark = pytest.mark.skipif(
    not (WEFTFS_CLI.is_file() and os.access(WEFTFS_CLI, os.X_OK)),
    reason="weftfs CLI not found at %s; run this test in the Docker python court"
    % WEFTFS_CLI,
)

torch = pytest.importorskip("torch") if HAS_TORCH else None
dcp = pytest.importorskip("torch.distributed.checkpoint") if HAS_TORCH else None

requires_torch = pytest.mark.skipif(
    not HAS_TORCH, reason="PyTorch is not installed in this court"
)

#: A state dict big enough that one save takes tens of milliseconds, so an
#: "immediately after submit" admission attempt really does overlap it.
STATE_SEED = 20261105


def make_state(seed=STATE_SEED, big=True):
    generator = torch.Generator().manual_seed(seed)
    state = {
        "embed": torch.randn(32, 32, generator=generator),
        "bias": torch.randn(32, generator=generator),
        "block.weight": torch.randn(64, 32, generator=generator),
    }
    if big:
        state["pad"] = torch.randn(256 * 256, generator=generator)
    return state


@pytest.fixture()
def store(tmp_path):
    root = tmp_path / "store"
    init_store(root, weftfs_cli=WEFTFS_CLI)
    return root


def _run(*args, expect=0):
    proc = subprocess.run(
        [str(WEFTFS_CLI), "--store", str(args[0]), *[str(a) for a in args[1:]]],
        capture_output=True,
        check=False,
    )
    if expect is not None:
        assert proc.returncode == expect, (
            "weftfs %s failed (%d): %s"
            % (" ".join(str(a) for a in args[1:]), proc.returncode, proc.stderr.decode())
        )
    return proc


def checkpoint_rows(store):
    proc = _run(store, "checkpoint", "list", "--json")
    return json.loads(proc.stdout.decode())["checkpoints"]


def checkpoint_names(store):
    return sorted(row["name"] for row in checkpoint_rows(store))


def assert_all_complete_and_exact(store, expected_by_name=None, tolerate_orphan_commit=False):
    """Every exposed checkpoint is complete, verifies, and restores its bytes."""
    rows = checkpoint_rows(store)
    for row in rows:
        assert row["complete"] is True, row
        result = verify_checkpoint(store, row["name"], full=True, weftfs_cli=WEFTFS_CLI)
        problems = result["full"].get("problems") or []
        if tolerate_orphan_commit and problems:
            # A crash between the commit-file fsync and the superblock write
            # leaves the old, complete state visible and an orphan commit file
            # above it. Readers are correct; the store *reports* the orphan
            # instead of hiding it. That must be the only problem.
            assert all(
                "above the highest published superblock" in str(problem.get("detail", ""))
                for problem in problems
            ), problems
        else:
            assert result["full"]["ok"] is True, (row["name"], problems)
        if expected_by_name is None:
            continue
        expected = expected_by_name[row["name"]]
        target = {name: torch.zeros_like(value) for name, value in expected.items()}
        from weftcodec.dcp import WeftStorageReader

        reader = WeftStorageReader(row["name"], store_root=store, weftfs_cli=WEFTFS_CLI)
        try:
            dcp.load(target, storage_reader=reader, no_dist=True)
        finally:
            reader.close()
        for name, value in expected.items():
            assert torch.equal(target[name], value), (row["name"], name)
    return rows


def session_for(store, **kwargs):
    kwargs.setdefault("staging_bytes", 64 << 20)
    kwargs.setdefault("session_idle_secs", 0)
    kwargs.setdefault("weftfs_cli", str(WEFTFS_CLI))
    return CheckpointSession(str(store), **kwargs)


# ---------------------------------------------------------------------------
# Estimation and admission
# ---------------------------------------------------------------------------


@requires_torch
def test_admission_estimate_is_logical_bytes_without_copying():
    state = make_state(big=False)
    expected = sum(value.numel() * value.element_size() for value in state.values())
    estimated, unpriced = _estimate_state_bytes(state)
    assert estimated == expected
    assert unpriced == 0
    nested = {"a": [state, {"b": b"12345"}]}
    estimated, unpriced = _estimate_state_bytes(nested)
    assert estimated == expected + 5
    assert unpriced == 0


@requires_torch
def test_queue_full_is_typed_counted_and_publishes_nothing(store):
    """bound + 1: the extra save is refused by name, and the store is untouched."""
    session = session_for(
        store, max_outstanding=1, admission_policy="fail", admission_wait_ms=0.0
    )
    state = make_state()
    first = session.submit(state, checkpoint_id="first")
    with pytest.raises(QueueFull) as guarded:
        session.submit(state, checkpoint_id="second")
    error = guarded.value
    assert error.max_outstanding == 1
    assert error.outstanding == 1
    assert "max_outstanding=1" in str(error)
    assert "nothing was published" in str(error)
    assert session.stats["admission_refusals"] == 1
    # The refused name never appears, even after the admitted save completes.
    first.result(timeout=600)
    report = session.close()
    assert "second" not in checkpoint_names(store)
    assert report["completed"] == 1
    assert report["no_silent_skips"] is True
    assert_all_complete_and_exact(store)


@requires_torch
def test_blocking_policy_waits_then_admits(store):
    """The blocking policy is a delay, not a refusal: every save lands."""
    session = session_for(
        store, max_outstanding=1, admission_policy="block", admission_wait_ms=30000.0
    )
    tickets = []
    state = make_state(big=False)
    for index in range(4):
        tickets.append(session.submit(state, checkpoint_id="c%d" % index))
    for ticket in tickets:
        ticket.result(timeout=600)
    report = session.close()
    assert report["admitted"] == 4
    assert report["completed"] == 4
    assert report["cancelled_before_start"] == 0
    assert report["no_silent_skips"] is True
    assert session.stats["admission_refusals"] == 0
    assert session.stats["queue_high_water"] == 1
    assert session.stats["admission_waits"] >= 3
    assert session.stats["admission_wait_ms_max"] > 0.0
    assert sorted(checkpoint_names(store)) == ["c0", "c1", "c2", "c3"]


@requires_torch
def test_byte_bound_refuses_with_both_numbers(store):
    """A byte bound refuses with the bound and the request in the message."""
    state = make_state(big=False)
    request = _estimate_state_bytes(state)[0]
    session = session_for(
        store,
        max_outstanding=None,
        max_staged_bytes=request + 1,
        admission_policy="fail",
        admission_wait_ms=0.0,
    )
    first = session.submit(state, checkpoint_id="one")
    with pytest.raises(QueueFull) as guarded:
        session.submit(state, checkpoint_id="two")
    assert guarded.value.byte_budget == request + 1
    assert guarded.value.requested_bytes == request
    assert guarded.value.staged_bytes == request
    first.result(timeout=600)
    session.close()
    assert checkpoint_names(store) == ["one"]


@requires_torch
def test_an_oversize_save_is_admitted_alone_rather_than_refused_forever(store):
    """A save larger than max_staged_bytes degenerates to one outstanding save."""
    state = make_state(big=False)
    request = _estimate_state_bytes(state)[0]
    session = session_for(
        store,
        max_outstanding=4,
        max_staged_bytes=max(1, request // 2),
        admission_policy="fail",
        admission_wait_ms=0.0,
    )
    first = session.submit(state, checkpoint_id="one")
    with pytest.raises(QueueFull):
        session.submit(state, checkpoint_id="two")
    first.result(timeout=600)
    second = session.submit(state, checkpoint_id="two")
    second.result(timeout=600)
    session.close()
    assert checkpoint_names(store) == ["one", "two"]


@requires_torch
def test_submit_after_close_is_a_typed_error(store):
    session = session_for(store, max_outstanding=2)
    session.close()
    with pytest.raises(QueueClosed):
        session.submit(make_state(big=False), checkpoint_id="late")


# ---------------------------------------------------------------------------
# Ordering and accounting
# ---------------------------------------------------------------------------


@requires_torch
def test_saves_publish_in_submission_order(store):
    """Distinct ids restore their own bytes and generations climb in order.

    One worker is the only publisher, so no later save can overtake an earlier
    one. Two independent observations prove it: each save's restored content is
    exactly the state that was submitted at that position, and the store's own
    published generation counter is strictly increasing in submission order.

    (A *base* name resolves to its newest version by design -- the reader's
    `_resolve_storage_name` -- so versions are not used as the ordering
    witness here.)
    """
    session = session_for(store, max_outstanding=3, admission_policy="block")
    states = [make_state(seed=STATE_SEED + index, big=False) for index in range(3)]
    tickets = [
        session.submit(states[index], checkpoint_id="s%d" % index)
        for index in range(3)
    ]
    for ticket in tickets:
        ticket.result(timeout=600)
    report = session.close()
    assert report["no_silent_skips"] is True
    names = checkpoint_names(store)
    assert names == ["s0", "s1", "s2"], names
    generations = [
        int((ticket.stats.get("finish") or {})["generation"]) for ticket in tickets
    ]
    assert generations == sorted(generations), generations
    assert len(set(generations)) == 3, generations
    # Each checkpoint holds exactly the state submitted at its position.
    assert_all_complete_and_exact(
        store,
        {"s0": states[0], "s1": states[1], "s2": states[2]},
    )


@requires_torch
def test_no_silent_skips_under_load(store):
    session = session_for(
        store, max_outstanding=2, admission_policy="block", admission_wait_ms=30000.0
    )
    state = make_state(big=False)
    count = 12
    tickets = [
        session.submit(state, checkpoint_id="load") for _ in range(count)
    ]
    for ticket in tickets:
        ticket.result(timeout=900)
    report = session.close()
    assert report["admitted"] == count
    assert report["completed"] == count
    assert report["failed"] == 0
    assert report["cancelled_before_start"] == 0
    assert report["unfinished"] == 0
    assert report["no_silent_skips"] is True
    assert session.stats["queue_high_water"] <= 2
    assert len(checkpoint_names(store)) == count


@requires_torch
def test_drain_reports_every_admitted_save(store):
    session = session_for(store, max_outstanding=4, admission_policy="fail")
    state = make_state(big=False)
    tickets = [
        session.submit(state, checkpoint_id="d%d" % index) for index in range(4)
    ]
    report = session.close()
    assert report["admitted"] == 4
    assert report["completed"] == 4
    assert report["no_silent_skips"] is True
    assert len(report["tickets"]) == 4
    assert {ticket["status"] for ticket in report["tickets"]} == {"completed"}
    assert [ticket["index"] for ticket in report["tickets"]] == [1, 2, 3, 4]
    for ticket in tickets:
        assert ticket.status == ticket.COMPLETED
    # A second close returns the same report and does not re-run anything.
    again = session.close()
    assert again["admitted"] == 4
    assert len(checkpoint_names(store)) == 4


@requires_torch
def test_close_without_wait_cancels_queued_saves_only(store):
    """`close(wait=False)` refuses to start queued saves; both are accounted."""
    session = session_for(store, max_outstanding=8, admission_policy="block")
    state = make_state()
    tickets = [
        session.submit(state, checkpoint_id="q%d" % index) for index in range(5)
    ]
    report = session.close(wait=False)
    assert report["admitted"] == 5
    assert report["no_silent_skips"] is True
    assert report["timed_out"] is False
    started = [ticket for ticket in tickets if ticket.status == ticket.COMPLETED]
    cancelled = [ticket for ticket in tickets if ticket.status == ticket.CANCELLED]
    assert started, "the in-flight save must complete, not be abandoned"
    assert cancelled, "queued saves must be cancelled, not silently dropped"
    assert len(started) + len(cancelled) == 5
    for ticket in cancelled:
        with pytest.raises(SaveNotStarted):
            ticket.result()
    # Only the saves that ran are in the store, and they are exact.
    names = checkpoint_names(store)
    assert names == sorted(ticket.checkpoint_id for ticket in started)
    assert_all_complete_and_exact(store)


@requires_torch
def test_a_cancelled_ticket_never_publishes(store):
    session = session_for(store, max_outstanding=8, admission_policy="block")
    state = make_state()
    first = session.submit(state, checkpoint_id="keep")
    second = session.submit(state, checkpoint_id="drop")
    assert second.cancel() is True
    assert second.cancel() is False
    assert second.status == second.CANCELLED
    first.result(timeout=600)
    report = session.close()
    assert report["completed"] == 1
    assert report["cancelled_before_start"] == 1
    assert report["no_silent_skips"] is True
    assert checkpoint_names(store) == ["keep"]


# ---------------------------------------------------------------------------
# Fencing
# ---------------------------------------------------------------------------


@requires_torch
def test_two_sessions_keep_the_writer_fence(store):
    """The second session is refused (Busy); it is never interleaved."""
    first = session_for(store, max_outstanding=2, session_idle_secs=30)
    state = make_state(big=False)
    first.submit(state, checkpoint_id="owner").result(timeout=600)

    second = session_for(store, max_outstanding=2, session_idle_secs=30)
    ticket = second.submit(state, checkpoint_id="intruder")
    with pytest.raises(BaseException) as guarded:
        ticket.result(timeout=600)
    text = str(guarded.value)
    class_name = getattr(guarded.value, "class_name", "")
    assert "busy" in text.lower() or class_name == "Busy", text
    assert ticket.status == ticket.FAILED
    second_report = second.close()
    assert second_report["failed"] == 1
    assert second_report["no_silent_skips"] is True
    assert "intruder" not in checkpoint_names(store)

    # Once the owner releases the lock the same session id can be used again.
    first.close()
    third = session_for(store, max_outstanding=2, session_idle_secs=30)
    third.submit(state, checkpoint_id="intruder").result(timeout=600)
    third.close()
    assert "intruder" in checkpoint_names(store)
    assert_all_complete_and_exact(store)


# ---------------------------------------------------------------------------
# Faults through the queue
# ---------------------------------------------------------------------------


CHILD_TEMPLATE = """
import sys, time
sys.path.insert(0, {root!r})
import torch
from weftcodec.dcp import CheckpointSession
generator = torch.Generator().manual_seed({seed})
state = {{
    "embed": torch.randn(32, 32, generator=generator),
    "bias": torch.randn(32, generator=generator),
    "block.weight": torch.randn(64, 32, generator=generator),
    "pad": torch.randn(256 * 256, generator=generator),
}}
session = CheckpointSession(
    {store!r},
    weftfs_cli={cli!r},
    session_idle_secs=0,
    max_outstanding={bound},
    admission_policy="block",
    admission_wait_ms=30000.0,
)
{body}
"""


def _child_env(phase=None):
    env = {**os.environ, "PYTHONPATH": PYTHON_ROOT}
    env.pop("WEFT_CRASH_PHASE", None)
    if phase is not None:
        env["WEFT_CRASH_PHASE"] = phase
    return env


@requires_torch
def test_killed_before_a_queued_save_starts_publishes_nothing(store):
    """SIGKILL after both are admitted: at most the first, never partial."""
    body = (
        "session.submit(state, checkpoint_id='first')\n"
        "print('ADMITTED-FIRST', flush=True)\n"
        "session.submit(state, checkpoint_id='second')\n"
        "print('ADMITTED-SECOND', flush=True)\n"
        "time.sleep(60)\n"
    )
    script = CHILD_TEMPLATE.format(
        root=PYTHON_ROOT, seed=STATE_SEED, store=str(store), cli=str(WEFTFS_CLI), bound=4, body=body
    )
    proc = subprocess.Popen(
        [sys.executable, "-c", script],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=_child_env(),
    )
    try:
        deadline = time.time() + 120
        seen = []
        while time.time() < deadline:
            line = proc.stdout.readline().decode()
            if not line:
                break
            seen.append(line.strip())
            if "ADMITTED-SECOND" in line:
                break
        assert any("ADMITTED-SECOND" in line for line in seen), seen
        proc.kill()
        proc.wait(timeout=60)
    finally:
        if proc.poll() is None:
            proc.kill()
    time.sleep(0.3)
    names = checkpoint_names(store)
    assert "second" not in names, names
    # Whatever was published (possibly nothing) is whole and exact.
    assert_all_complete_and_exact(store)


@requires_torch
def test_crash_phases_through_the_queue_leave_old_or_new(store):
    """The store's own publication phases, driven through the queue worker."""
    body = "session.save(state, checkpoint_id='crash')\n"
    script = CHILD_TEMPLATE.format(
        root=PYTHON_ROOT, seed=STATE_SEED, store=str(store), cli=str(WEFTFS_CLI), bound=2, body=body
    )
    baseline = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        timeout=300,
        env=_child_env(),
    )
    assert baseline.returncode == 0, baseline.stdout + baseline.stderr

    for phase, expect_new in (
        ("after_objects", False),
        ("after_commit_file", False),
        ("after_superblock_write", True),
    ):
        proc = subprocess.run(
            [sys.executable, "-c", script],
            capture_output=True,
            timeout=300,
            env=_child_env(phase),
        )
        assert proc.returncode != 0, "phase %s did not abort" % phase
        rows = checkpoint_rows(store)
        names = {row["name"]: row for row in rows}
        assert "crash" in names, "phase %s lost the baseline checkpoint" % phase
        # Old-or-new: a phase before the superblock must expose no new
        # checkpoint; the phase after it must expose the new one whole.
        assert ("crash.v2" in names) is expect_new, (phase, sorted(names))
        for name, row in names.items():
            assert row["complete"] is True, (phase, row)
            result = verify_checkpoint(store, name, full=True, weftfs_cli=WEFTFS_CLI)
            problems = result["full"].get("problems") or []
            if phase == "after_commit_file":
                # Old state is exact; the abort landed after the commit-file
                # fsync and before the superblock, so the orphan commit file is
                # reported. It must be the only problem.
                assert result["full"]["ok"] is False, problems
                assert problems, "the orphan commit file was not reported"
                assert all(
                    "above the highest published superblock" in str(
                        problem.get("detail", "")
                    )
                    for problem in problems
                ), problems
            else:
                assert result["full"]["ok"] is True, problems
        # Old-or-new: the whole store still reads as one complete state.
        assert_all_complete_and_exact(
            store, tolerate_orphan_commit=(phase == "after_commit_file")
        )
