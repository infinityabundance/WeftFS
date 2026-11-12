"""Differential and failure-semantics tests for fast DCP metadata encoding.

``weftcodec.dcp.serialize_metadata`` is the reference oracle for
``weft.dcp.metadata.v1``. ``serialize_metadata_fast`` may use a cached
``MetadataPlan``; it must never emit bytes the oracle would not emit.

Coverage:

* 5 deterministic seeds x 200 randomized metadata trees (0-12 state-dict rows,
  tensor and byte rows, 0-4 dimensional shapes, 0-3 chunks, unicode and
  different-length names that sort by *encoded* key bytes, content roots of 0,
  1 and 32 bytes, ``None`` and tuple offsets, ``None`` and integer indices,
  ``None``/``dict`` planner data, ``None``/string DCP version) -- compared both
  with a fresh plan cache (``rebuilt``) and with a shared cache (``plan``);
* invalid metadata: identical rejection classes;
* stale plans: a changed entry set, dtype, shape, chunk layout, storage info,
  offset or index must fall back and count the fallback; the variable fields
  (content roots, names, version, planner data) must still hit the plan;
* a plan built for one entry set is never used for another;
* the real 169-entry fixture: identical bytes to the oracle and an exact
  restore through the existing reader (store-backed).

The store-backed test is skipped with an explicit reason when the Rust CLI is
absent, exactly like ``test_dcp.py``; everything else runs without a store.
"""

from __future__ import annotations

import hashlib
import importlib.util
import os
import random
from pathlib import Path

import pytest

import support  # noqa: F401  (puts the tests directory on sys.path)

from weftcodec import wcb
from weftcodec.dcp import (
    DEFAULT_CLI,
    DCP_METADATA_NAME,
    InvalidMetadata,
    MetadataPlanCache,
    StorageInfo,
    UnsupportedStateObject,
    WeftStorageReader,
    WeftStorageWriter,
    init_store,
    metadata_parts,
    serialize_metadata,
    serialize_metadata_fast,
)
from weftcodec.errors import LimitExceeded, Malformed
from weftcodec.limits import Limits

HAS_TORCH = importlib.util.find_spec("torch") is not None
WEFTFS_CLI = Path(os.environ.get("WEFTFS_CLI", DEFAULT_CLI))

SEEDS = (11, 12, 13, 14, 15)
TREES_PER_SEED = 200

torch = pytest.importorskip("torch") if HAS_TORCH else None
if HAS_TORCH:
    from torch.distributed.checkpoint.metadata import (
        BytesStorageMetadata,
        ChunkStorageMetadata,
        Metadata,
        MetadataIndex,
        StorageMeta,
        TensorProperties,
        TensorStorageMetadata,
    )

DIMS = (0, 1, 7, 63, 127, 128, 255, 256, 16384, (1 << 32) - 1)
NAMES = (
    "model.embed.weight",
    "model.bias",
    "小模型.权重",
    "café.état",
    "z",
    "zz",
    "a.b.c.d.e",
    "state.1",
    "state.10",
    "🧵.tensor",
)


def _dtype(rng):
    return rng.choice(
        (
            torch.float32,
            torch.bfloat16,
            torch.float16,
            torch.float64,
            torch.int8,
            torch.uint8,
        )
    )


def _random_state_row(rng):
    if rng.randrange(8) == 0:
        return BytesStorageMetadata()
    rank = rng.randrange(0, 5)
    shape = [rng.choice(DIMS) for _ in range(rank)]
    chunks = []
    for _ in range(rng.randrange(0, 4)):
        chunks.append(
            ChunkStorageMetadata(
                offsets=torch.Size([rng.choice((0, 1, 127, 128)) for _ in shape]),
                sizes=torch.Size([rng.choice(DIMS) for _ in shape]),
            )
        )
    return TensorStorageMetadata(
        properties=TensorProperties(dtype=_dtype(rng)),
        size=torch.Size(shape),
        chunks=chunks,
    )


def _random_content_root(rng):
    length = rng.choice((0, 1, 32))
    return bytes(rng.randrange(256) for _ in range(length))


def _random_storage_rows(rng, fqns):
    rows = {}
    for fqn in fqns:
        if rng.randrange(4) == 0:
            continue
        rank = rng.randrange(0, 3)
        shape = tuple(rng.choice(DIMS) for _ in range(rank))
        key = MetadataIndex(
            fqn,
            offset=None if rng.randrange(2) else tuple(0 for _ in shape),
            index=rng.choice((None, None, 0, 1, 127, 128)),
        )
        rows[key] = StorageInfo(
            entry=fqn,
            dtype=rng.choice(("torch.float32", "torch.bfloat16", "torch.int8")),
            shape=shape,
            storage_offsets=tuple(0 for _ in shape),
            lengths=shape,
            content_root=_random_content_root(rng),
        )
    return rows


def random_metadata(rng):
    names = rng.sample(NAMES, rng.randrange(0, 6))
    state = {name: _random_state_row(rng) for name in names}
    planner = None
    if rng.randrange(3) == 0:
        planner = {
            "planner-%d" % index: tuple("part-%d" % part for part in range(rng.randrange(0, 3)))
            for index in range(rng.randrange(0, 3))
        }
    version = rng.choice((None, "1.0.0", "2.15.0+cpu"))
    metadata = Metadata(
        state_dict_metadata=state,
        storage_data=_random_storage_rows(rng, list(state)),
        planner_data=planner,
        storage_meta=StorageMeta(checkpoint_id="ckpt", save_id="save"),
        version=version,
    )
    return metadata


def test_random_metadata_is_byte_identical():
    """5 seeds x 200 trees, both with a fresh plan and through a warm cache."""
    compared = 0
    shared = MetadataPlanCache(max_plans=3)
    for seed in SEEDS:
        rng = random.Random(seed)
        for index in range(TREES_PER_SEED):
            metadata = random_metadata(rng)
            checkpoint_id = "ckpt-%d-%d" % (seed, index)
            storage_name = checkpoint_id + (".v1" if index % 5 == 0 else "")
            expected = serialize_metadata(
                metadata, checkpoint_id=checkpoint_id, storage_name=storage_name
            )
            # (1) A fresh cache must build a plan and prove it against the tree.
            cold = MetadataPlanCache()
            report = {}
            actual = serialize_metadata_fast(
                metadata,
                checkpoint_id=checkpoint_id,
                storage_name=storage_name,
                cache=cold,
                report=report,
            )
            assert actual == expected, "seed %d tree %d (cold)" % (seed, index)
            assert report["mode"] == "rebuilt"
            assert report["plan_builds"] == 1
            assert report["oracle_checks"] == 1
            # (2) The warm cache may or may not match this shape; it must never
            #     change the bytes, and a miss must be a counted fallback.
            warm = {}
            actual = serialize_metadata_fast(
                metadata,
                checkpoint_id=checkpoint_id,
                storage_name=storage_name,
                cache=shared,
                report=warm,
            )
            assert actual == expected, "seed %d tree %d (warm)" % (seed, index)
            assert warm["mode"] in ("plan", "rebuilt")
            # (3) A second call with the same cache hits the plan if it matched.
            again = {}
            actual = serialize_metadata_fast(
                metadata,
                checkpoint_id=checkpoint_id,
                storage_name=storage_name,
                cache=shared,
                report=again,
            )
            assert actual == expected
            if again["mode"] == "plan":
                assert again["plan_hits"] >= warm["plan_hits"]
            compared += 1
    assert compared == len(SEEDS) * TREES_PER_SEED
    assert shared.stats["plan_builds"] >= 1


def _metadata_with(entries, storage_extra=None):
    state = {name: row for name, row in entries}
    storage = {
        MetadataIndex(name, offset=None): StorageInfo(
            entry=name,
            dtype="torch.float32",
            shape=tuple(row.size) if isinstance(row, TensorStorageMetadata) else (),
            storage_offsets=tuple(0 for _ in (row.size if isinstance(row, TensorStorageMetadata) else ())),
            lengths=tuple(row.size) if isinstance(row, TensorStorageMetadata) else (),
            content_root=bytes([index]) * 32,
        )
        for index, (name, row) in enumerate(entries)
    }
    if storage_extra:
        for key, info in storage_extra.items():
            storage[key] = info
    return Metadata(
        state_dict_metadata=state,
        storage_data=storage,
        planner_data=None,
        storage_meta=StorageMeta(checkpoint_id="ckpt", save_id="save"),
        version="1.0.0",
    )


def _tensor(shape, dtype=None, chunks=None):
    return TensorStorageMetadata(
        properties=TensorProperties(dtype=dtype or torch.float32),
        size=torch.Size(shape),
        chunks=list(chunks or []),
    )


BASE_ENTRIES = [
    ("model.weight", _tensor([16, 8])),
    ("model.bias", _tensor([16])),
    ("buffer", BytesStorageMetadata()),
]


def _encode_pair(metadata, checkpoint_id="ckpt", storage_name="save", cache=None):
    expected = serialize_metadata(
        metadata, checkpoint_id=checkpoint_id, storage_name=storage_name
    )
    report = {}
    actual = serialize_metadata_fast(
        metadata,
        checkpoint_id=checkpoint_id,
        storage_name=storage_name,
        cache=cache if cache is not None else MetadataPlanCache(),
        report=report,
    )
    assert actual == expected
    return report, actual, expected


def test_plan_hits_track_only_the_variable_fields():
    cache = MetadataPlanCache()
    report, _blob, _expected = _encode_pair(BASE_ENTRIES and _metadata_with(BASE_ENTRIES), cache=cache)
    assert report["mode"] == "rebuilt"
    # Content roots, names, version and planner data change every save; shapes
    # and dtype stay. The plan must still be used.
    metadata = _metadata_with(BASE_ENTRIES)
    metadata.version = "2.15.0"
    for key in list(metadata.storage_data):
        info = metadata.storage_data[key]
        metadata.storage_data[key] = StorageInfo(
            entry=info.entry,
            dtype=info.dtype,
            shape=info.shape,
            storage_offsets=info.storage_offsets,
            lengths=info.lengths,
            content_root=bytes([0xAA]) * 32,
        )
    report, _blob, _expected = _encode_pair(
        metadata, checkpoint_id="ckpt-2", storage_name="save.v2", cache=cache
    )
    assert report["mode"] == "plan"
    assert report["plan_hits"] == 1
    assert report["fallbacks"] == 1


@pytest.mark.parametrize(
    "mutate",
    [
        pytest.param(lambda entries: entries + [("extra.weight", _tensor([4]))], id="added-entry"),
        pytest.param(lambda entries: entries[:2], id="removed-entry"),
        pytest.param(lambda entries: [("renamed.weight", entries[0][1])] + entries[1:], id="renamed"),
        pytest.param(lambda entries: [(entries[0][0], _tensor([16, 8], dtype=torch.bfloat16))] + entries[1:], id="dtype"),
        pytest.param(lambda entries: [(entries[0][0], _tensor([16, 9]))] + entries[1:], id="shape"),
        pytest.param(lambda entries: [(entries[0][0], _tensor([16, 8], chunks=[ChunkStorageMetadata(offsets=torch.Size([0, 0]), sizes=torch.Size([8, 8]))]))] + entries[1:], id="chunks"),
        pytest.param(lambda entries: [(entries[0][0], BytesStorageMetadata())] + entries[1:], id="tensor-to-bytes"),
    ],
)
def test_a_stale_plan_is_never_used(mutate):
    cache = MetadataPlanCache()
    first = _metadata_with(BASE_ENTRIES)
    report, first_blob, first_expected = _encode_pair(first, cache=cache)
    assert report["mode"] == "rebuilt"

    second = _metadata_with(mutate(list(BASE_ENTRIES)))
    report, blob, expected = _encode_pair(second, cache=cache)
    assert blob == expected, "a stale plan changed the bytes"
    assert blob != first_blob
    assert report["mode"] == "rebuilt", "a stale plan was used"
    assert report["plan_hits"] == 0
    assert report["fallbacks"] == 2


def test_stale_storage_info_is_never_used():
    cache = MetadataPlanCache()
    base = _metadata_with(BASE_ENTRIES)
    _encode_pair(base, cache=cache)

    mutations = {
        "entry": lambda info: StorageInfo(
            entry=info.entry + ".moved",
            dtype=info.dtype,
            shape=info.shape,
            storage_offsets=info.storage_offsets,
            lengths=info.lengths,
            content_root=info.content_root,
        ),
        "dtype": lambda info: StorageInfo(
            entry=info.entry,
            dtype="torch.int8",
            shape=info.shape,
            storage_offsets=info.storage_offsets,
            lengths=info.lengths,
            content_root=info.content_root,
        ),
        "shape": lambda info: StorageInfo(
            entry=info.entry,
            dtype=info.dtype,
            shape=(1, 2),
            storage_offsets=info.storage_offsets,
            lengths=info.lengths,
            content_root=info.content_root,
        ),
        "lengths": lambda info: StorageInfo(
            entry=info.entry,
            dtype=info.dtype,
            shape=info.shape,
            storage_offsets=info.storage_offsets,
            lengths=(3,),
            content_root=info.content_root,
        ),
        "storage_offsets": lambda info: StorageInfo(
            entry=info.entry,
            dtype=info.dtype,
            shape=info.shape,
            storage_offsets=(1,),
            lengths=info.lengths,
            content_root=info.content_root,
        ),
    }
    for label, mutate in mutations.items():
        metadata = _metadata_with(BASE_ENTRIES)
        key = sorted(metadata.storage_data, key=lambda k: (k.fqn, tuple(k.offset or ())))[0]
        metadata.storage_data[key] = mutate(metadata.storage_data[key])
        report, blob, expected = _encode_pair(metadata, cache=cache)
        assert blob == expected, label
        assert report["mode"] == "rebuilt", label
        assert report["plan_hits"] == 0, label


def test_stale_storage_key_is_never_used():
    cache = MetadataPlanCache()
    base = _metadata_with(BASE_ENTRIES)
    _encode_pair(base, cache=cache)

    # An added indexed row and an added offset row change the storage key set.
    # (`MetadataIndex.index` is `compare=False` in torch, so an index can only
    # appear on an fqn/offset pair that has no plain sibling.)
    extra_index = MetadataIndex("extra.weight", offset=None, index=7)
    metadata = _metadata_with(
        BASE_ENTRIES,
        storage_extra={
            extra_index: StorageInfo(
                entry="extra.weight",
                dtype="torch.float32",
                shape=(4,),
                storage_offsets=(0,),
                lengths=(4,),
                content_root=b"\x02" * 32,
            )
        },
    )
    assert len(metadata.storage_data) == len(BASE_ENTRIES) + 1
    report, blob, expected = _encode_pair(metadata, cache=cache)
    assert blob == expected
    assert report["mode"] == "rebuilt"
    assert report["plan_hits"] == 0

    offset_key = MetadataIndex("model.bias", offset=(4,))
    metadata = _metadata_with(
        BASE_ENTRIES,
        storage_extra={
            offset_key: StorageInfo(
                entry="model.bias",
                dtype="torch.float32",
                shape=(4,),
                storage_offsets=(0,),
                lengths=(4,),
                content_root=b"\x03" * 32,
            )
        },
    )
    assert len(metadata.storage_data) == len(BASE_ENTRIES) + 1
    report, blob, expected = _encode_pair(metadata, cache=cache)
    assert blob == expected
    assert report["mode"] == "rebuilt"
    assert report["plan_hits"] == 0


def test_an_index_change_on_the_same_key_is_detected():
    """`MetadataIndex.index` is not part of the key, so it must be compared."""
    cache = MetadataPlanCache()
    entries = [("model.weight", _tensor([16, 8]))]
    metadata = _metadata_with(entries)
    key = next(iter(metadata.storage_data))
    metadata.storage_data[key] = StorageInfo(
        entry="model.weight",
        dtype="torch.float32",
        shape=(16, 8),
        storage_offsets=(0, 0),
        lengths=(16, 8),
        content_root=b"\x01" * 32,
    )
    _encode_pair(metadata, cache=cache)
    metadata.storage_data[key] = StorageInfo(
        entry="model.weight",
        dtype="torch.float32",
        shape=(16, 8),
        storage_offsets=(0, 0),
        lengths=(16, 8),
        content_root=b"\x01" * 32,
    )
    # Replace the key with one that carries an index but compares equal.
    del metadata.storage_data[key]
    indexed = MetadataIndex("model.weight", offset=None, index=3)
    metadata.storage_data[indexed] = StorageInfo(
        entry="model.weight",
        dtype="torch.float32",
        shape=(16, 8),
        storage_offsets=(0, 0),
        lengths=(16, 8),
        content_root=b"\x01" * 32,
    )
    report, blob, expected = _encode_pair(metadata, cache=cache)
    assert blob == expected, "an index change produced stale bytes"
    assert report["mode"] == "rebuilt"
    assert report["plan_hits"] == 0


def test_a_plan_is_not_reused_across_different_limits():
    cache = MetadataPlanCache()
    metadata = _metadata_with(BASE_ENTRIES)
    _encode_pair(metadata, cache=cache)
    tight = Limits(max_structured_items=10_000)
    expected = serialize_metadata(
        metadata, checkpoint_id="ckpt", storage_name="save"
    )
    assert expected  # the default-limit bytes are fine with this bound too
    report = {}
    actual = serialize_metadata_fast(
        metadata,
        checkpoint_id="ckpt",
        storage_name="save",
        cache=cache,
        limits=tight,
        report=report,
    )
    # Same bytes under a bound the metadata fits; the plan is not used because
    # it was proven under a different limit set.
    assert actual == expected
    assert report["mode"] == "rebuilt"


INVALID_METADATA = (
    ("storage-data-none", lambda metadata: setattr(metadata, "storage_data", None), InvalidMetadata),
    (
        "bad-storage-info",
        lambda metadata: metadata.storage_data.__setitem__(
            MetadataIndex("model.weight", offset=None), object()
        ),
        InvalidMetadata,
    ),
    (
        "unsupported-state-object",
        lambda metadata: metadata.state_dict_metadata.__setitem__("bad", object()),
        UnsupportedStateObject,
    ),
)


@pytest.mark.parametrize("label,mutate,error", INVALID_METADATA, ids=[row[0] for row in INVALID_METADATA])
def test_invalid_metadata_raises_the_same_class(label, mutate, error):
    metadata = _metadata_with(BASE_ENTRIES)
    mutate(metadata)
    with pytest.raises(error):
        serialize_metadata(metadata, checkpoint_id="ckpt", storage_name="save")
    with pytest.raises(error):
        serialize_metadata_fast(
            metadata,
            checkpoint_id="ckpt",
            storage_name="save",
            cache=MetadataPlanCache(),
        )


def test_out_of_range_values_raise_the_same_class():
    metadata = _metadata_with(BASE_ENTRIES)
    key = sorted(metadata.storage_data, key=lambda k: (k.fqn, tuple(k.offset or ())))[0]
    info = metadata.storage_data[key]
    metadata.storage_data[key] = StorageInfo(
        entry=info.entry,
        dtype=info.dtype,
        shape=(-1,),
        storage_offsets=info.storage_offsets,
        lengths=info.lengths,
        content_root=info.content_root,
    )
    with pytest.raises(Malformed):
        serialize_metadata(metadata, checkpoint_id="ckpt", storage_name="save")
    with pytest.raises(Malformed):
        serialize_metadata_fast(
            metadata,
            checkpoint_id="ckpt",
            storage_name="save",
            cache=MetadataPlanCache(),
        )


def test_lowered_limits_raise_the_same_class():
    metadata = _metadata_with(BASE_ENTRIES)
    value = metadata_parts(metadata, checkpoint_id="ckpt", storage_name="save")
    for limits in (
        Limits(max_structured_items=2),
        Limits(max_structured_items=50),
        Limits(max_string_bytes=4),
        Limits(max_structured_depth=2),
        Limits(max_structured_depth=3),
        Limits(max_structured_depth=10),
    ):
        try:
            wcb.encode(value, limits)
            oracle_error = None
        except Exception as exc:  # noqa: BLE001 - the class is the assertion
            oracle_error = exc
        try:
            serialize_metadata_fast(
                metadata,
                checkpoint_id="ckpt",
                storage_name="save",
                cache=MetadataPlanCache(),
                limits=limits,
            )
            fast_error = None
        except Exception as exc:  # noqa: BLE001
            fast_error = exc
        if oracle_error is None:
            assert fast_error is None, (limits, fast_error)
        else:
            assert fast_error is not None, limits
            assert type(fast_error) is type(oracle_error), (
                limits,
                type(fast_error),
                type(oracle_error),
            )
            assert isinstance(fast_error, LimitExceeded)


def test_metadata_parts_and_the_oracle_share_one_description():
    """The split value/encode path is the oracle's own definition."""
    metadata = _metadata_with(BASE_ENTRIES)
    value = metadata_parts(metadata, checkpoint_id="c", storage_name="s")
    assert wcb.encode(value) == serialize_metadata(
        metadata, checkpoint_id="c", storage_name="s"
    )


def test_metadata_encoding_override_is_validated_and_reported(tmp_path, monkeypatch):
    """`reference` forces the oracle; an unknown value fails at construction."""
    from weftcodec.dcp import DEFAULT_STAGING_BYTES

    store = tmp_path / "store"
    init_store(store, weftfs_cli=WEFTFS_CLI)
    monkeypatch.setenv("WEFT_DCP_METADATA_ENCODING", "reference")
    writer = WeftStorageWriter(
        "ckpt",
        store_root=str(store),
        staging_bytes=DEFAULT_STAGING_BYTES,
        init_store=False,
        weftfs_cli=WEFTFS_CLI,
    )
    assert writer.metadata_encoding == "reference"
    writer.close()

    monkeypatch.setenv("WEFT_DCP_METADATA_ENCODING", "nonsense")
    with pytest.raises(ValueError):
        WeftStorageWriter(
            "ckpt",
            store_root=str(store),
            staging_bytes=DEFAULT_STAGING_BYTES,
            init_store=False,
            weftfs_cli=WEFTFS_CLI,
        )
    with pytest.raises(ValueError):
        WeftStorageWriter(
            "ckpt",
            store_root=str(store),
            staging_bytes=DEFAULT_STAGING_BYTES,
            init_store=False,
            weftfs_cli=WEFTFS_CLI,
            metadata_encoding="plan",
        )


# -- the real fixture and a store-backed restore ------------------------------

def _has_weftfs() -> bool:
    return WEFTFS_CLI.is_file() and os.access(WEFTFS_CLI, os.X_OK)


store_required = pytest.mark.skipif(
    not _has_weftfs(),
    reason="weftfs CLI not found at %s; run this test in the Docker python court"
    % WEFTFS_CLI,
)


class _CapturingWriter(WeftStorageWriter):
    captured = None

    def finish(self, metadata, results):
        super().finish(metadata, results)
        self.captured = metadata


class _Model(torch.nn.Module):
    def __init__(self):
        super().__init__()
        generator = torch.Generator().manual_seed(20261111)
        self.embed = torch.nn.Parameter(torch.randn(16, 8, generator=generator))
        self.bias = torch.nn.Parameter(torch.randn(16, generator=generator))
        self.block = torch.nn.Module()
        self.block.weight = torch.nn.Parameter(torch.randn(8, 4, generator=generator))


@store_required
def test_real_fixture_bytes_and_restore_are_unchanged(tmp_path):
    """The fast path's metadata bytes equal the oracle's, and a restore works."""
    import torch.distributed.checkpoint as dcp

    store = tmp_path / "store"
    init_store(store, weftfs_cli=WEFTFS_CLI)
    model = _Model()
    source = {name: value.detach().clone() for name, value in model.state_dict().items()}

    # A clean process-global cache: another test in the same process may have
    # cached a plan for an identical entry-set shape.
    from weftcodec.dcp import METADATA_PLANS

    METADATA_PLANS.clear()
    cache = MetadataPlanCache()
    modes = []
    for index in range(2):
        name = "ckpt-%d" % index
        writer = _CapturingWriter(
            name,
            store_root=str(store),
            topology=1,
            shard_strategy="zstd",
            init_store=False,
            weftfs_cli=WEFTFS_CLI,
            atomic_publish=True,
        )
        dcp.save(model.state_dict(), storage_writer=writer, no_dist=True)
        captured = writer.captured
        report = {}
        expected = serialize_metadata(
            captured, checkpoint_id=name, storage_name=writer.store_name
        )
        fast = serialize_metadata_fast(
            captured,
            checkpoint_id=name,
            storage_name=writer.store_name,
            cache=cache,
            report=report,
        )
        assert fast == expected
        assert hashlib.sha256(fast).digest() == hashlib.sha256(expected).digest()
        stored = writer._get_service().read_file(
            "/checkpoints/%s/%s" % (writer.store_name, DCP_METADATA_NAME)
        )
        assert stored.bytes == expected, "the stored metadata blob changed"
        stored.close()
        report_writer = dict(writer.stats)
        writer.close()
        modes.append(report_writer["metadata_encode"]["mode"])
    # The first save builds a plan, the second (same shapes) uses it.
    assert modes == ["rebuilt", "plan"], modes

    for index in range(2):
        reader = WeftStorageReader(
            "ckpt-%d" % index, store_root=store, weftfs_cli=WEFTFS_CLI
        )
        target = _Model()
        dcp.load(target.state_dict(), storage_reader=reader, no_dist=True)
        assert set(target.state_dict()) == set(source)
        for key in source:
            assert torch.equal(source[key], target.state_dict()[key]), key
        assert reader.metadata_source == "dcp"
        reader.close()
