"""Record-family validators written from the published specification text.

Every validator takes canonical bytes, refuses noncanonical input with a typed
`WcbError`, and returns the computed identity where the schema defines one.
Nothing here imports or shells out to the Rust, Go or Python Weft packages.

Where a rule was not stated precisely enough to implement without reading the
reference code, the validator documents the gap in its docstring and in the
`UNSPECIFIED` list. Those notes are findings, not workarounds hidden in code.
"""

from __future__ import annotations

from wcb import (
    Bytes,
    I64,
    Timestamp,
    U64,
    WcbError,
    decode_and_reencode,
    encode,
    expect_array,
    expect_bytes,
    expect_map,
    expect_map_value,
    expect_text,
    expect_u64,
)
from hashing import hash_domain

# ---------------------------------------------------------------------------
# Domains

DOMAIN_SELECTOR = "weft/selector/v1"
DOMAIN_CONTEXT_ROOT = "weft/context-root/v1"
DOMAIN_CAUSAL = "weft/causal/v1"
DOMAIN_TENSOR_VALUES = "weft/tensor/values/v1"
DOMAIN_TENSOR_LOGICAL = "weft/tensor/logical/v1"
DOMAIN_TENSOR_SEMANTIC = "weft/tensor/semantic/v1"
DOMAIN_TENSOR_SEMANTIC_VALUES = "weft/tensor/semantic-values/v1"
DOMAIN_PROVENANCE = "weft/provenance/v1"
DOMAIN_CAPABILITY = "weft/capability/v1"
DOMAIN_CAPABILITY_ENVELOPE = "weft/capability/envelope/v1"
DOMAIN_CAPABILITY_SIGN = "weft/capability/sign/v1"
DOMAIN_CAPABILITY_BLOB = "weft/capability/blob/v1"
DOMAIN_KV_PROV = "weft/kv/prov/v1"
DOMAIN_KV_ADAPTERS = "weft/kv/adapters/v1"
DOMAIN_KV_TOKENIZER = "weft/kv/tokenizer/v1"

DTYPE_SIZES = {
    "f16": 2, "bf16": 2, "f32": 4, "f64": 8,
    "i8": 1, "u8": 1, "i16": 2, "i32": 4, "i64": 8,
}
QUANTIZED_DTYPES = {"q8_0": 1, "q4_0": 1}
SEMANTIC_DTYPES = {"bf16", "fp16", "fp32", "q8_0", "q4_0"}

MAX_TENSOR_RANK = 8
MAX_TENSOR_ELEMENTS = 1 << 32
MAX_CAUSAL_ENTRIES = 4096

UNSPECIFIED = [
    "Selector value typing: `WEFT-SELECTORS.md` §2 says a `value` is a WCB "
    "scalar and that timestamps use tag 0x09 'when the runtime value is "
    "temporal', but the text grammar has no timestamp literal and does not say "
    "whether a decoded `attr_eq` timestamp is accepted. This verifier accepts "
    "text, u64, i64, bool, null and timestamp; that choice is not stated in "
    "the selector document.",
    "Selector namespace grammar is not restated in `WEFT-SELECTORS.md`; this "
    "verifier applies the namespace grammar from `WEFT-CONTEXT-ROOT.md` §3.1.",
    "Context Root `causal` array ordering is defined in `WEFT-CAUSAL.md` §1.1, "
    "not in the Context Root record table. A reader that implements only "
    "`WEFT-CONTEXT-ROOT.md` cannot validate the array.",
    "KV provenance §3.2 says the `page` map is the mode-12 descriptor "
    "'verbatim' but does not restate its key types or cross-checks. This "
    "verifier had to read `WEFT-AI-CODEC.md` §13.2/§13.4 to implement it; the "
    "KV document is not self-contained.",
    "`WEFT-PROVENANCE.md` §2 lists the record fields but never says whether an "
    "unknown top-level key is rejected. Every other frozen record schema says "
    "so explicitly. This verifier rejects one (NonCanonical) and records the "
    "choice; the spec should state it.",
    "`WEFT-PROVENANCE.md` §2 shows `inputs[].object` as an optional hint but "
    "does not say whether an absent hint is encoded as `null` or by omitting "
    "the key. This verifier accepts both forms and does not normalise them.",
    "The frozen hash-domain table in `WEFTCODEC-FORMAT.md` §1.1 omits several "
    "domains that code and vectors use, including "
    "`weft/checkpoint/manifest/v1`, `weft/index/state/v1`, `weft/pack/*`, "
    "`weft/load/*` and `weft/mw2/key/v1`.",
    "`WEFT-TENSOR.md` §3 fixes the logical record key set and error classes, "
    "but the `values_hash` preimage for `q8_0`/`q4_0` is defined in §2.1 with "
    "a separate scale/zero-point byte concatenation. A logical-record-only "
    "reader cannot check the hash without the tensor layer.",
]

# ---------------------------------------------------------------------------
# Shared helpers


def _hex(value) -> str:
    return value.hex() if isinstance(value, (bytes, bytearray)) else value


def check_root(raw, where: str, allow_zero: bool = False) -> bytes:
    data = expect_bytes(raw, 32, where)
    if not allow_zero and not any(data):
        raise WcbError("Malformed", "%s is all zero" % where)
    return data


def nc_root(raw, where: str) -> bytes:
    """A 32-byte root whose type/length/zero failures are NonCanonical."""
    if not isinstance(raw, Bytes):
        raise WcbError("NonCanonical", "%s is not bytes" % where)
    if len(raw.value) != 32:
        raise WcbError("NonCanonical", "%s is not 32 bytes" % where)
    if not any(raw.value):
        raise WcbError("NonCanonical", "%s is all zero" % where)
    return raw.value


def record_map(value, required, allowed, where: str) -> dict:
    """A record map whose missing/extra keys are NonCanonical."""
    if not isinstance(value, dict):
        raise WcbError("Malformed", "%s is not a map" % where)
    missing = set(required) - set(value)
    extra = set(value) - set(allowed)
    if missing or extra:
        raise WcbError(
            "NonCanonical",
            "%s key set differs (missing %s, extra %s)"
            % (where, sorted(missing), sorted(extra)),
        )
    return value


def _sorted_unique_roots(values, where: str, allow_empty=True) -> list:
    items = []
    previous = None
    for index, item in enumerate(expect_array(values, where)):
        raw = expect_bytes(item, 32, "%s[%d]" % (where, index))
        if not any(raw):
            raise WcbError("NonCanonical", "%s[%d] is all zero" % (where, index))
        if previous is not None:
            if raw == previous:
                raise WcbError("NonCanonical", "%s has a duplicate" % where)
            if raw < previous:
                raise WcbError("NonCanonical", "%s is not sorted" % where)
        previous = raw
        items.append(raw)
    if not items and not allow_empty:
        raise WcbError("NonCanonical", "%s may not be empty" % where)
    return items


def _sorted_unique_text(values, where: str) -> list:
    items = []
    previous = None
    for index, item in enumerate(expect_array(values, where)):
        text = expect_text(item, "%s[%d]" % (where, index))
        if previous is not None:
            if text == previous:
                raise WcbError("NonCanonical", "%s has a duplicate" % where)
            if text.encode("utf-8") < previous.encode("utf-8"):
                raise WcbError("NonCanonical", "%s is not sorted" % where)
        previous = text
        items.append(text)
    return items


def _namespace(text, where: str) -> str:
    text = expect_text(text, where)
    try:
        raw = text.encode("ascii", "strict")
    except UnicodeEncodeError:
        raise WcbError("NonCanonical", "%s is not ASCII" % where)
    if not 1 <= len(raw) <= 128:
        raise WcbError("NonCanonical", "%s length is not 1..=128" % where)
    if text in (".", ".."):
        raise WcbError("NonCanonical", "%s is a reserved name" % where)
    first = text[0]
    if not (first.isalpha() or first.isdigit() or first == "_"):
        raise WcbError("NonCanonical", "%s first byte is invalid" % where)
    for char in text[1:]:
        if not (char.isalnum() or char in "_.-"):
            raise WcbError("NonCanonical", "%s contains an invalid byte" % where)
    return text


def _resolver(text) -> str:
    text = expect_text(text, "resolver")
    try:
        raw = text.encode("ascii", "strict")
    except UnicodeEncodeError:
        raise WcbError("NonCanonical", "resolver is not ASCII")
    if not 1 <= len(raw) <= 64:
        raise WcbError("NonCanonical", "resolver length is not 1..=64")
    if not ("a" <= text[0].lower() <= "z"):
        raise WcbError("NonCanonical", "resolver must start with an ASCII letter")
    for char in text[1:]:
        if not (char.isalnum() or char in "_.-"):
            raise WcbError("NonCanonical", "resolver contains an invalid byte")
    return text


def _validate_budget(value) -> None:
    budget = expect_map_value(value, "budget")
    allowed = {"max_results", "max_bytes", "max_depth", "max_work"}
    if not budget:
        raise WcbError("NonCanonical", "budget is empty")
    for key, item in budget.items():
        if key not in allowed:
            raise WcbError("NonCanonical", "budget has unknown key %r" % key)
        if expect_u64(item, "budget.%s" % key) < 1:
            raise WcbError("NonCanonical", "budget.%s is zero" % key)


# ---------------------------------------------------------------------------
# Selector


_EXPR_ARITY = {
    "true": 1, "false": 1,
    "has": 2, "tag": 2, "relation": 3,
    "attr_eq": 3, "attr_ne": 3, "attr_lt": 3, "attr_le": 3,
    "attr_gt": 3, "attr_ge": 3, "not": 2, "and": 2, "or": 2,
}


def _scalar(value) -> bool:
    return (
        value is None
        or isinstance(value, (str, U64, I64, Timestamp))
        or isinstance(value, bool)
    )


def _validate_expr(value, depth: int = 0) -> None:
    if depth > 64:
        raise WcbError("LimitExceeded", "selector expression too deep")
    expr = expect_array(value, "predicate")
    if not expr or not isinstance(expr[0], str):
        raise WcbError("NonCanonical", "predicate is not a discriminated array")
    op = expr[0]
    if op not in _EXPR_ARITY:
        raise WcbError("NonCanonical", "unknown predicate %r" % op)
    if len(expr) != _EXPR_ARITY[op]:
        raise WcbError("NonCanonical", "predicate %r has the wrong arity" % op)
    if op in ("true", "false"):
        return
    if op in ("has", "tag"):
        if not expect_text(expr[1], "predicate.%s" % op):
            raise WcbError("NonCanonical", "empty name")
        return
    if op == "relation":
        expect_text(expr[1], "predicate.relation")
        check_root(expr[2], "predicate.relation target")
        return
    if op.startswith("attr_"):
        expect_text(expr[1], "predicate.field")
        if not _scalar(expr[2]):
            raise WcbError("NonCanonical", "predicate comparison is not a scalar")
        return
    if op in ("and", "or"):
        operands = expect_array(expr[1], "predicate.%s" % op)
        if len(operands) < 2:
            raise WcbError("NonCanonical", "%s needs two operands" % op)
        for operand in operands:
            _validate_expr(operand, depth + 1)
        return
    if op == "not":
        _validate_expr(expr[1], depth + 1)
        return
    raise WcbError("NonCanonical", "unreachable predicate")


def validate_selector(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    key_map = record_map(
        value,
        {"schema", "namespace", "predicate"},
        {"schema", "namespace", "predicate", "order", "limit", "offset"},
        "selector",
    )
    if expect_text(key_map["schema"], "schema") != "weft.selector.v1":
        raise WcbError("UnknownSchema", "selector schema mismatch")
    _namespace(key_map["namespace"], "namespace")
    _validate_expr(key_map["predicate"])
    if "order" in key_map:
        order = expect_array(key_map["order"], "order")
        seen = set()
        for item in order:
            entry = expect_map(item, {"field", "dir"}, "order entry")
            field = expect_text(entry["field"], "order.field")
            direction = expect_text(entry["dir"], "order.dir")
            if direction not in ("asc", "desc"):
                raise WcbError("NonCanonical", "order dir is not asc/desc")
            if field in seen:
                raise WcbError("NonCanonical", "duplicate order field")
            seen.add(field)
    if "limit" in key_map:
        expect_u64(key_map["limit"], "limit")
    if "offset" in key_map:
        if expect_u64(key_map["offset"], "offset") == 0:
            raise WcbError("NonCanonical", "offset 0 must be omitted")
    return {
        "selector_id": hash_domain(DOMAIN_SELECTOR, raw),
        "namespace": key_map["namespace"],
        "schema": "weft.selector.v1",
    }


# ---------------------------------------------------------------------------
# Context Root


def validate_context_root(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    required = {
        "schema", "namespace", "selector", "selector_id", "snapshot",
        "resolver", "causal", "ordering",
    }
    allowed = required | {"capability_hash", "budget"}
    if not isinstance(value, dict):
        raise WcbError("Malformed", "context root is not a map")
    unknown = set(value) - allowed
    if unknown:
        raise WcbError("NonCanonical", "context root has unknown keys %s" % sorted(unknown))
    if set(required) - set(value):
        raise WcbError("NonCanonical", "context root is missing required keys")
    if expect_text(value["schema"], "schema") != "weft.context-root.v1":
        raise WcbError("UnknownSchema", "context root schema mismatch")
    namespace = _namespace(value["namespace"], "namespace")
    _resolver(value["resolver"])
    nc_root(value["snapshot"], "snapshot")
    _sorted_unique_roots(value["causal"], "causal")
    ordering = expect_text(value["ordering"], "ordering")
    if ordering != "selector_then_object_id":
        raise WcbError("UnsupportedVersion", "unknown ordering policy")
    if "capability_hash" in value:
        nc_root(value["capability_hash"], "capability_hash")
    if "budget" in value:
        _validate_budget(value["budget"])

    selector_bytes = expect_bytes(value["selector"], None, "selector")
    selector = validate_selector(selector_bytes)
    if selector["namespace"] != namespace:
        raise WcbError("NonCanonical", "selector namespace differs from the root")
    recorded = nc_root(value["selector_id"], "selector_id")
    if recorded != selector["selector_id"]:
        raise WcbError("NonCanonical", "selector_id does not hash the selector")
    return {
        "context_root_id": hash_domain(DOMAIN_CONTEXT_ROOT, raw),
        "schema": "weft.context-root.v1",
    }


# ---------------------------------------------------------------------------
# Causal closure


def validate_causal(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    key_map = record_map(
        value, {"schema", "parents", "closure"},
        {"schema", "parents", "closure"}, "causal record",
    )
    if expect_text(key_map["schema"], "schema") != "weft.causal.v1":
        raise WcbError("UnknownSchema", "causal schema mismatch")
    parents = _sorted_unique_roots(key_map["parents"], "parents")
    closure = _sorted_unique_roots(key_map["closure"], "closure")
    if len(closure) > MAX_CAUSAL_ENTRIES or len(parents) > MAX_CAUSAL_ENTRIES:
        raise WcbError("LimitExceeded", "causal entry bound exceeded")
    closure_set = set(closure)
    for parent in parents:
        if parent not in closure_set:
            raise WcbError("NonCanonical", "closure is missing a parent")
    return {"closure_hash": hash_domain(DOMAIN_CAUSAL, raw), "schema": "weft.causal.v1"}


# ---------------------------------------------------------------------------
# Logical tensor and semantic root


def _shape(value) -> tuple:
    shape = expect_array(value, "shape")
    if not 1 <= len(shape) <= MAX_TENSOR_RANK:
        if len(shape) == 0:
            raise WcbError("NonCanonical", "rank 0 is not a logical tensor")
        raise WcbError("LimitExceeded", "rank above max_tensor_rank")
    extents = []
    product = 1
    for index, item in enumerate(shape):
        extent = expect_u64(item, "shape[%d]" % index)
        if extent == 0:
            raise WcbError("NonCanonical", "zero extent")
        if product > (1 << 64) // extent:
            raise WcbError("LimitExceeded", "shape product overflows u64")
        product *= extent
        extents.append(extent)
    if product > MAX_TENSOR_ELEMENTS:
        raise WcbError("LimitExceeded", "product above max_tensor_elements")
    return tuple(extents), product


def validate_logical_tensor(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    key_map = record_map(
        value,
        {"schema", "dtype", "shape", "element_count", "values_hash"},
        {"schema", "dtype", "shape", "element_count", "values_hash"},
        "logical tensor",
    )
    if expect_text(key_map["schema"], "schema") != "weft.tensor.logical.v1":
        raise WcbError("UnknownSchema", "logical tensor schema mismatch")
    dtype = expect_text(key_map["dtype"], "dtype")
    if dtype not in DTYPE_SIZES and dtype not in QUANTIZED_DTYPES:
        raise WcbError("UnsupportedFeature", "unknown dtype %r" % dtype)
    _extents, product = _shape(key_map["shape"])
    if expect_u64(key_map["element_count"], "element_count") != product:
        raise WcbError("NonCanonical", "element_count differs from the shape product")
    values_hash = nc_root(key_map["values_hash"], "values_hash")
    if dtype in DTYPE_SIZES and product * DTYPE_SIZES[dtype] > (1 << 64) - 1:
        raise WcbError("LimitExceeded", "element bytes overflow")
    return {
        "logical_root": hash_domain(DOMAIN_TENSOR_LOGICAL, raw),
        "values_hash": values_hash,
        "schema": "weft.tensor.logical.v1",
    }


def _policy(value):
    policy = expect_map_value(value, "policy")
    scheme = expect_text(policy.get("scheme"), "policy.scheme")
    if scheme == "none":
        expect_map(policy, {"scheme"}, "policy")
        return scheme, None
    if scheme == "mantissa_truncate":
        expect_map(policy, {"scheme", "bits"}, "policy")
        bits = expect_u64(policy["bits"], "policy.bits")
        if bits < 1:
            raise WcbError("Malformed", "mantissa bits must be >= 1")
        return scheme, bits
    if scheme == "block_quantize":
        expect_map(policy, {"scheme", "block_size", "levels"}, "policy")
        block = expect_u64(policy["block_size"], "policy.block_size")
        levels = expect_u64(policy["levels"], "policy.levels")
        if block < 1 or levels < 1:
            raise WcbError("Malformed", "block_size and levels must be >= 1")
        return scheme, (block, levels)
    raise WcbError("Malformed", "unknown semantic policy %r" % scheme)


SEMANTIC_DTYPE_SIZES = {"bf16": 2, "fp16": 2, "fp32": 4}


def _mantissa_truncate(data: bytes, dtype: str, bits: int) -> bytes:
    if dtype not in ("bf16", "fp16", "fp32"):
        raise WcbError("Malformed", "mantissa_truncate needs a float dtype")
    width = SEMANTIC_DTYPE_SIZES[dtype]
    mantissa_bits = {"bf16": 7, "fp16": 10, "fp32": 23}[dtype]
    if not 1 <= bits <= mantissa_bits:
        raise WcbError("Malformed", "mantissa bits out of range")
    mask = (1 << bits) - 1
    exponent_mask = {2: 0x7F80, 4: 0x7F800000}[width]
    out = bytearray()
    for offset in range(0, len(data), width):
        word = int.from_bytes(data[offset:offset + width], "little")
        if word & exponent_mask != exponent_mask:
            word &= ~mask
        out.extend(word.to_bytes(width, "little"))
    return bytes(out)


def _block_quantize(data: bytes, dtype: str, block: int, levels: int) -> bytes:
    if dtype not in ("bf16", "fp16", "fp32"):
        raise WcbError("Malformed", "block_quantize needs a float dtype")
    width = SEMANTIC_DTYPE_SIZES[dtype]
    count = len(data) // width
    if count % block != 0:
        raise WcbError("Malformed", "block_size does not divide the element count")
    signed = [
        int.from_bytes(data[i * width:(i + 1) * width], "little", signed=True)
        for i in range(count)
    ]
    out = []
    sign_bit = 1 << (width * 8 - 1)
    for start in range(0, count, block):
        values = signed[start:start + block]
        amax = max(abs(v) for v in values)
        if amax == 0 or levels == 1:
            out.extend([0] * len(values))
            continue
        reconstructed = []
        for value in values:
            k = ((value + amax) * (levels - 1) + amax) // (2 * amax)
            k = min(max(k, 0), levels - 1)
            level = -amax + ((2 * amax * k + (levels - 1) // 2) // (levels - 1))
            reconstructed.append(level)
        out.extend(reconstructed)
    return b"".join(v.to_bytes(width, "little", signed=True) for v in out)


def validate_semantic_root(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    key_map = expect_map(
        value,
        {"schema", "dtype", "shape", "element_count", "policy",
         "quantized_values_hash"},
        "semantic root",
    )
    if expect_text(key_map["schema"], "schema") != "weft.tensor.semantic.v1":
        raise WcbError("UnknownSchema", "semantic root schema mismatch")
    dtype = expect_text(key_map["dtype"], "dtype")
    if dtype not in SEMANTIC_DTYPES:
        raise WcbError("Malformed", "unknown semantic dtype")
    _extents, product = _shape(key_map["shape"])
    if expect_u64(key_map["element_count"], "element_count") != product:
        raise WcbError("Malformed", "element_count differs from the shape product")
    scheme, parameter = _policy(key_map["policy"])
    if scheme == "mantissa_truncate":
        mantissa_bits = {"bf16": 7, "fp16": 10, "fp32": 23}.get(dtype)
        if mantissa_bits is None or parameter > mantissa_bits:
            raise WcbError("Malformed", "mantissa bits are out of range for the dtype")
    if scheme == "block_quantize":
        block, levels = parameter
        if product % block != 0:
            raise WcbError("Malformed", "block_size does not divide element_count")
        if levels > (1 << 24):
            raise WcbError("Malformed", "levels above 2^24")
    check_root(key_map["quantized_values_hash"], "quantized_values_hash")
    return {
        "semantic_root": hash_domain(DOMAIN_TENSOR_SEMANTIC, raw),
        "dtype": dtype,
        "element_count": product,
        "scheme": scheme,
        "parameter": parameter,
        "quantized_values_hash": key_map["quantized_values_hash"].value,
        "schema": "weft.tensor.semantic.v1",
    }


def verify_semantic_values(values_bytes: bytes, record: dict) -> None:
    scheme = record["scheme"]
    dtype = record["dtype"]
    if scheme == "none":
        expected = hash_domain(DOMAIN_TENSOR_VALUES, values_bytes)
    elif scheme == "mantissa_truncate":
        expected = hash_domain(
            DOMAIN_TENSOR_SEMANTIC_VALUES,
            _mantissa_truncate(values_bytes, dtype, record["parameter"]),
        )
    elif scheme == "block_quantize":
        expected = hash_domain(
            DOMAIN_TENSOR_SEMANTIC_VALUES,
            _block_quantize(values_bytes, dtype, *record["parameter"]),
        )
    else:  # pragma: no cover - _policy rejects anything else
        raise AssertionError("unknown scheme")
    if expected != record["quantized_values_hash"]:
        raise WcbError("Malformed", "quantized_values_hash does not match the values")


# ---------------------------------------------------------------------------
# Tensor layout descriptor (no standalone identity)


def validate_tensor_layout(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    if not isinstance(value, dict):
        raise WcbError("Malformed", "layout descriptor is not a map")
    required = {"schema", "dtype", "shape", "layout"}
    unknown = set(value) - required - {"block_shape", "axis", "shards", "index"}
    if unknown:
        raise WcbError("NonCanonical", "layout has unknown keys %s" % sorted(unknown))
    if set(required) - set(value):
        raise WcbError("NonCanonical", "layout is missing required keys")
    if expect_text(value["schema"], "schema") != "weft.tensor.layout.v1":
        raise WcbError("UnknownSchema", "layout schema mismatch")
    dtype = expect_text(value["dtype"], "dtype")
    if dtype not in DTYPE_SIZES and dtype not in QUANTIZED_DTYPES:
        raise WcbError("UnsupportedFeature", "unknown dtype")
    extents, _product = _shape(value["shape"])
    layout = expect_text(value["layout"], "layout")
    if layout == "row-major":
        if set(value) != required:
            raise WcbError("NonCanonical", "row-major carries layout fields")
    elif layout == "column-major":
        if set(value) != required:
            raise WcbError("NonCanonical", "column-major carries layout fields")
    elif layout == "blocked":
        if set(value) != required | {"block_shape"}:
            raise WcbError("NonCanonical", "blocked key set is wrong")
        blocks = expect_array(value["block_shape"], "block_shape")
        if len(blocks) != len(extents):
            raise WcbError("NonCanonical", "block_shape rank differs")
        for index, item in enumerate(blocks):
            if expect_u64(item, "block_shape[%d]" % index) < 1:
                raise WcbError("NonCanonical", "zero block extent")
    elif layout == "sharded":
        if set(value) != required | {"axis", "shards", "index"}:
            raise WcbError("NonCanonical", "sharded key set is wrong")
        axis = expect_u64(value["axis"], "axis")
        shards = expect_u64(value["shards"], "shards")
        index = expect_u64(value["index"], "index")
        if axis >= len(extents) or index >= shards or shards < 1:
            raise WcbError("NonCanonical", "shard parameters are inconsistent")
        if extents[axis] % shards != 0:
            raise WcbError("NonCanonical", "shape[axis] is not divisible by shards")
    else:
        raise WcbError("UnsupportedFeature", "unknown layout %r" % layout)
    return {"schema": "weft.tensor.layout.v1", "layout": layout}


# ---------------------------------------------------------------------------
# Provenance


def validate_provenance(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    required = {
        "schema", "operation", "params", "inputs", "outputs", "environment",
        "determinism", "principal", "timestamp_ns",
    }
    if not isinstance(value, dict):
        raise WcbError("Malformed", "provenance is not a map")
    if set(value) != required:
        missing = sorted(required - set(value))
        extra = sorted(set(value) - required)
        # The spec does not say extra keys are rejected; see UNSPECIFIED.
        raise WcbError(
            "NonCanonical" if not missing else "Malformed",
            "provenance key set differs (missing %s, extra %s)" % (missing, extra),
        )
    if expect_text(value["schema"], "schema") != "weft.provenance.v1":
        raise WcbError("UnknownSchema", "provenance schema mismatch")
    operation = expect_text(value["operation"], "operation")
    if not operation or len(operation.encode("utf-8")) > 1024:
        raise WcbError("NonCanonical", "operation is empty or too long")
    params = expect_map_value(value["params"], "params")
    if len(encode(params)) > 65536:
        raise WcbError("LimitExceeded", "params exceed their bound")
    inputs = expect_array(value["inputs"], "inputs")
    if len(inputs) > 1024:
        raise WcbError("LimitExceeded", "too many inputs")
    previous = None
    for index, item in enumerate(inputs):
        if not isinstance(item, dict):
            raise WcbError("Malformed", "input is not a map")
        if set(item) not in ({"role", "content"}, {"role", "content", "object"}):
            raise WcbError("Malformed", "input key set is wrong")
        role = expect_text(item["role"], "input.role")
        if not role or len(role.encode("utf-8")) > 1024:
            raise WcbError("NonCanonical", "input role is empty or too long")
        content = check_root(item["content"], "input.content")
        if "object" in item and item["object"] is not None:
            check_root(item["object"], "input.object")
        key = (role.encode("utf-8"), content)
        if previous is not None and key <= previous:
            raise WcbError("NonCanonical", "inputs are not strictly ordered")
        previous = key
    outputs = _sorted_unique_roots(value["outputs"], "outputs")
    if len(outputs) > 1024:
        raise WcbError("LimitExceeded", "too many outputs")
    environment = expect_map_value(value["environment"], "environment")
    for key in ("tool", "version"):
        if key not in environment:
            raise WcbError("Malformed", "environment is missing %s" % key)
        text = expect_text(environment[key], "environment.%s" % key)
        if not text or len(text.encode("utf-8")) > 1024:
            raise WcbError("NonCanonical", "environment.%s is empty or too long" % key)
    if "env_hash" in environment and environment["env_hash"] is not None:
        check_root(environment["env_hash"], "environment.env_hash")
    if len(encode(environment)) > 16384:
        raise WcbError("LimitExceeded", "environment exceeds its bound")
    determinism = expect_text(value["determinism"], "determinism")
    if determinism not in ("exact", "declared", "unknown"):
        raise WcbError("NonCanonical", "unknown determinism class")
    principal = expect_map(value["principal"], {"uid", "gid"}, "principal")
    expect_u64(principal["uid"], "principal.uid")
    expect_u64(principal["gid"], "principal.gid")
    if not isinstance(value["timestamp_ns"], Timestamp):
        raise WcbError("Malformed", "timestamp_ns is not a timestamp")
    if len(raw) > 1048576:
        raise WcbError("LimitExceeded", "record exceeds its bound")
    return {"provenance_id": hash_domain(DOMAIN_PROVENANCE, raw),
            "schema": "weft.provenance.v1"}


# ---------------------------------------------------------------------------
# Capability


def validate_capability_v1(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    required = {"schema", "principal", "namespaces", "roots"}
    allowed = required | {"predicates", "budget", "expires_at"}
    if not isinstance(value, dict):
        raise WcbError("Malformed", "capability is not a map")
    unknown = set(value) - allowed
    if unknown:
        raise WcbError("NonCanonical", "capability has unknown keys %s" % sorted(unknown))
    if set(required) - set(value):
        raise WcbError("NonCanonical", "capability is missing required keys")
    if expect_text(value["schema"], "schema") != "weft.capability.v1":
        raise WcbError("UnknownSchema", "capability schema mismatch")
    principal = expect_text(value["principal"], "principal")
    if not 1 <= len(principal.encode("utf-8")) <= 256:
        raise WcbError("NonCanonical", "principal length is not 1..=256")
    namespaces = _sorted_unique_text(value["namespaces"], "namespaces")
    roots = _sorted_unique_roots(value["roots"], "roots")
    if not namespaces and not roots:
        raise WcbError("NonCanonical", "a capability grants neither namespace nor root")
    if "predicates" in value:
        _sorted_unique_text(value["predicates"], "predicates")
    if "budget" in value:
        _validate_budget(value["budget"])
    if "expires_at" in value and not isinstance(value["expires_at"], Timestamp):
        raise WcbError("Malformed", "expires_at is not a timestamp")
    return {"capability_hash": hash_domain(DOMAIN_CAPABILITY, raw),
            "schema": "weft.capability.v1"}


ENVELOPE_KEYS = {
    "schema", "grantor", "grantee", "tenant", "roots", "namespaces",
    "predicates", "expires_at_ns", "budget", "revocation_id",
    "issued_at_ns", "nonce",
}


def validate_capability_envelope(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    key_map = expect_map(value, ENVELOPE_KEYS, "capability envelope")
    if expect_text(key_map["schema"], "schema") != "weft.capability.envelope.v1":
        raise WcbError("UnknownSchema", "envelope schema mismatch")
    grantor = check_root(key_map["grantor"], "grantor")
    check_root(key_map["grantee"], "grantee")
    tenant = expect_text(key_map["tenant"], "tenant")
    if not 1 <= len(tenant.encode("utf-8")) <= 256:
        raise WcbError("NonCanonical", "tenant length is not 1..=256")
    roots = _sorted_unique_roots(key_map["roots"], "roots")
    namespaces = _sorted_unique_text(key_map["namespaces"], "namespaces")
    if not roots and not namespaces:
        raise WcbError("NonCanonical", "envelope grants neither root nor namespace")
    _sorted_unique_text(key_map["predicates"], "predicates")
    expires = key_map["expires_at_ns"]
    issued = key_map["issued_at_ns"]
    if not isinstance(issued, Timestamp):
        raise WcbError("Malformed", "issued_at_ns is not a timestamp")
    if expires is not None:
        if not isinstance(expires, Timestamp):
            raise WcbError("Malformed", "expires_at_ns is not a timestamp or null")
        if expires.value < issued.value:
            raise WcbError("NonCanonical", "expiry precedes issuance")
    budget = key_map["budget"]
    if budget is not None:
        _validate_budget(budget)
    check_root(key_map["revocation_id"], "revocation_id")
    check_root(key_map["nonce"], "nonce")
    return {
        "envelope_id": hash_domain(DOMAIN_CAPABILITY_ENVELOPE, raw),
        "grantor": grantor,
        "grantee": key_map["grantee"].value,
        "revocation_id": key_map["revocation_id"].value,
        "expires_at_ns": None if expires is None else expires.value,
        "sign_input": hash_domain(DOMAIN_CAPABILITY_SIGN, raw),
        "schema": "weft.capability.envelope.v1",
    }


def validate_capability_blob(raw: bytes) -> dict:
    value = decode_and_reencode(raw)
    key_map = expect_map(value, {"envelope", "signature"}, "capability blob")
    envelope_raw = expect_bytes(key_map["envelope"], None, "envelope")
    signature = expect_bytes(key_map["signature"], 64, "signature")
    envelope = validate_capability_envelope(envelope_raw)
    return {
        "blob_id": hash_domain(DOMAIN_CAPABILITY_BLOB, raw),
        "envelope": envelope,
        "signature": signature,
        "envelope_raw": envelope_raw,
        "schema": "weft.capability.blob.v1",
    }


# ---------------------------------------------------------------------------
# KV provenance (mode 19 frame)

PAGE_KEYS = {
    "schema", "model", "layer", "session", "page_index", "page_count",
    "seq_start", "seq_end", "dtype", "head_count", "head_dim", "shape",
    "page_bytes", "plane_codec", "payload_len",
}
KV_PROV_KEYS = {
    "schema", "page", "model_root", "adapter_roots", "adapter_set_root",
    "tokenizer", "tokenizer_root", "attention", "positional", "layout",
    "salt", "extensions", "payload_hash",
}
ATTENTION_KEYS = {
    "head_count", "head_dim", "sliding_window", "rope_theta_bits",
    "rope_scale_bits",
}
LAYOUT_KEYS = {"dtype", "page_rows", "layer_count"}
PAGE_DTYPE_SIZES = {"bf16": 2, "fp16": 2, "fp32": 4}


def _crc32c_table():
    table = []
    for value in range(256):
        crc = value
        for _ in range(8):
            crc = (crc >> 1) ^ 0x82F63B78 if crc & 1 else crc >> 1
        table.append(crc)
    return table


_CRC32C_TABLE = _crc32c_table()


def crc32c(data: bytes) -> int:
    crc = 0xFFFFFFFF
    for byte in data:
        crc = _CRC32C_TABLE[(crc ^ byte) & 0xFF] ^ (crc >> 8)
    return crc ^ 0xFFFFFFFF


def _u16(data, offset):
    return int.from_bytes(data[offset:offset + 2], "little")


def _u32(data, offset):
    return int.from_bytes(data[offset:offset + 4], "little")


def _u64(data, offset):
    return int.from_bytes(data[offset:offset + 8], "little")


def parse_frame(raw: bytes) -> dict:
    """Structural WFT1 checks shared by mode 19 and the reserved-mode cases."""
    if len(raw) < 100:
        raise WcbError("Malformed", "short frame")
    if raw[:4] != b"WFT1":
        raise WcbError("Malformed", "magic")
    if raw[4] != 1 or raw[5] != 0:
        raise WcbError("UnsupportedVersion", "format version")
    if crc32c(raw[0:72] + raw[76:96]) != _u32(raw, 72):
        raise WcbError("Corrupt", "header crc")
    domain = raw[6]
    mode = raw[7]
    flags = _u16(raw, 8)
    decoded_len = _u64(raw, 12)
    body_len = _u64(raw, 20)
    dep_count = _u32(raw, 28)
    chunk_count = _u32(raw, 32)
    features = _u16(raw, 36)
    content_hash = raw[40:72]
    table_len = _u64(raw, 76)
    if _u16(raw, 38) or _u32(raw, 84) or _u64(raw, 88):
        raise WcbError("Malformed", "reserved header field is non-zero")
    if domain > 1:
        raise WcbError("Malformed", "domain above 1")
    if flags & ~0x000F or flags & 0x0008 or flags & 0x0002:
        raise WcbError("UnsupportedFeature", "unsupported flag")
    if features & ~0x0003:
        raise WcbError("UnsupportedFeature", "unsupported feature")
    if table_len != dep_count * 48 + chunk_count * 48:
        raise WcbError("Malformed", "table_len")
    if len(raw) != 96 + table_len + body_len + 4:
        raise WcbError("Malformed", "total length")
    if crc32c(raw[:-4]) != _u32(raw, len(raw) - 4):
        raise WcbError("Corrupt", "trailer crc")
    body_start = 96 + table_len
    body = raw[body_start:body_start + body_len]
    return {
        "domain": domain,
        "mode": mode,
        "flags": flags,
        "decoded_len": decoded_len,
        "body_len": body_len,
        "dep_count": dep_count,
        "chunk_count": chunk_count,
        "features": features,
        "content_hash": content_hash,
        "body": body,
    }


def _kv_roots(values, where: str) -> list:
    """KV provenance dimension roots: every failure class is `Malformed`."""
    items = []
    previous = None
    for index, item in enumerate(expect_array(values, where)):
        raw = expect_bytes(item, 32, "%s[%d]" % (where, index))
        if not any(raw):
            raise WcbError("Malformed", "%s[%d] is all zero" % (where, index))
        if previous is not None:
            if raw == previous:
                raise WcbError("Malformed", "%s has a duplicate" % where)
            if raw < previous:
                raise WcbError("Malformed", "%s is not sorted" % where)
        previous = raw
        items.append(raw)
    return items


def _validate_attention(value) -> dict:
    attention = expect_map_value(value, "attention")
    expect_map(attention, ATTENTION_KEYS, "attention")
    heads = expect_u64(attention["head_count"], "attention.head_count")
    head_dim = expect_u64(attention["head_dim"], "attention.head_dim")
    if heads == 0 or head_dim == 0:
        raise WcbError("Malformed", "attention head count or dim is zero")
    if heads > 0xFFFFFFFF or head_dim > 0xFFFFFFFF:
        raise WcbError("Malformed", "attention dimension exceeds u32")
    for key in ("rope_theta_bits", "rope_scale_bits"):
        bits = expect_u64(attention[key], "attention.%s" % key)
        exponent = (bits >> 52) & 0x7FF
        if exponent == 0x7FF:
            raise WcbError("Malformed", "attention rope bits are NaN or infinite")
    expect_u64(attention["sliding_window"], "attention.sliding_window")
    return attention


def validate_kv_prov_frame(raw: bytes) -> dict:
    frame = parse_frame(raw)
    if frame["mode"] != 19:
        # 20 (ENGRAM_SHARD) and 21 are not implemented by this verifier.
        raise WcbError("UnsupportedMode", "mode %d is not implemented" % frame["mode"])
    if frame["dep_count"] != 0 or frame["chunk_count"] != 0:
        raise WcbError("Malformed", "mode 19 must not carry tables")
    if frame["flags"] != 0:
        raise WcbError("Malformed", "mode 19 flags must be zero")
    body = frame["body"]
    if len(body) < 4:
        raise WcbError("Malformed", "body shorter than its descriptor length")
    desc_len = _u32(body, 0)
    if desc_len > len(body) - 4:
        raise WcbError("Malformed", "descriptor extends past the body")
    descriptor_raw = body[4:4 + desc_len]
    payload = body[4 + desc_len:]
    descriptor = decode_and_reencode(descriptor_raw)

    if not isinstance(descriptor, dict):
        raise WcbError("Malformed", "descriptor is not a map")
    if set(descriptor) != KV_PROV_KEYS:
        raise WcbError("Malformed", "descriptor key set differs from weft.ai.kv_prov.v1")
    if expect_text(descriptor["schema"], "schema") != "weft.ai.kv_prov.v1":
        raise WcbError("Malformed", "descriptor schema mismatch")

    page = expect_map_value(descriptor["page"], "page")
    expect_map(page, PAGE_KEYS, "page")
    if expect_text(page["schema"], "page.schema") != "weft.ai.kv_cache.v1":
        raise WcbError("Malformed", "page schema mismatch")
    if not isinstance(page["model"], str) or not isinstance(page["session"], str):
        raise WcbError("Malformed", "page model/session is not text")
    layer = expect_u64(page["layer"], "page.layer")
    page_index = expect_u64(page["page_index"], "page.page_index")
    page_count = expect_u64(page["page_count"], "page.page_count")
    seq_start = expect_u64(page["seq_start"], "page.seq_start")
    seq_end = expect_u64(page["seq_end"], "page.seq_end")
    dtype = expect_text(page["dtype"], "page.dtype")
    if dtype not in ("bf16", "fp16", "fp32"):
        raise WcbError("Malformed", "page dtype is unknown")
    head_count = expect_u64(page["head_count"], "page.head_count")
    head_dim = expect_u64(page["head_dim"], "page.head_dim")
    if head_count == 0 or head_dim == 0:
        raise WcbError("Malformed", "page head count or dim is zero")
    shape = expect_array(page["shape"], "page.shape")
    if len(shape) != 3:
        raise WcbError("Malformed", "page shape is not rank 3")
    page_rows = expect_u64(shape[0], "page.shape[0]")
    if expect_u64(shape[1], "page.shape[1]") != head_count:
        raise WcbError("Malformed", "page shape head_count mismatch")
    if expect_u64(shape[2], "page.shape[2]") != head_dim:
        raise WcbError("Malformed", "page shape head_dim mismatch")
    page_bytes = expect_u64(page["page_bytes"], "page.page_bytes")
    if page_bytes != page_rows * head_count * head_dim * PAGE_DTYPE_SIZES[dtype]:
        raise WcbError("Malformed", "page_bytes disagrees with the shape and dtype")
    if page_index >= page_count or seq_end < seq_start:
        raise WcbError("Malformed", "page index or sequence range is invalid")
    if expect_text(page["plane_codec"], "page.plane_codec") != "huffman-canonical-v1":
        raise WcbError("Malformed", "page plane codec is unknown")
    expect_u64(page["payload_len"], "page.payload_len")

    model_root = check_root(descriptor["model_root"], "model_root")
    adapter_roots = _kv_roots(descriptor["adapter_roots"], "adapter_roots")
    adapter_set = check_root(descriptor["adapter_set_root"], "adapter_set_root")
    if hash_domain(DOMAIN_KV_ADAPTERS, encode([Bytes(x) for x in adapter_roots])) != adapter_set:
        raise WcbError("Malformed", "adapter_set_root does not hash the adapter roots")

    tokenizer = expect_map_value(descriptor["tokenizer"], "tokenizer")
    expect_map(tokenizer, {"scheme", "vocab_size"}, "tokenizer")
    scheme = expect_text(tokenizer["scheme"], "tokenizer.scheme")
    vocab = expect_u64(tokenizer["vocab_size"], "tokenizer.vocab_size")
    if not scheme or vocab < 1:
        raise WcbError("Malformed", "tokenizer scheme or vocab_size is invalid")
    tokenizer_record = {"scheme": scheme, "vocab_size": U64(vocab)}
    tokenizer_root = check_root(descriptor["tokenizer_root"], "tokenizer_root")
    if hash_domain(DOMAIN_KV_TOKENIZER, encode(tokenizer_record)) != tokenizer_root:
        raise WcbError("Malformed", "tokenizer_root does not hash the tokenizer")

    attention = _validate_attention(descriptor["attention"])
    if expect_u64(attention["head_count"], "attention.head_count") != head_count:
        raise WcbError("Malformed", "attention head_count disagrees with the page")
    if expect_u64(attention["head_dim"], "attention.head_dim") != head_dim:
        raise WcbError("Malformed", "attention head_dim disagrees with the page")

    positional = expect_text(descriptor["positional"], "positional")
    if positional not in ("rope", "alibi", "none"):
        raise WcbError("Malformed", "unknown positional treatment")

    layout = expect_map_value(descriptor["layout"], "layout")
    expect_map(layout, LAYOUT_KEYS, "layout")
    if expect_text(layout["dtype"], "layout.dtype") != dtype:
        raise WcbError("Malformed", "layout dtype disagrees with the page")
    if expect_u64(layout["page_rows"], "layout.page_rows") != page_rows:
        raise WcbError("Malformed", "layout page_rows disagrees with the page")
    if expect_u64(layout["layer_count"], "layout.layer_count") < 1:
        raise WcbError("Malformed", "layout layer_count is zero")

    salt = check_root(descriptor["salt"], "salt", allow_zero=True)
    if len(salt) != 32:
        raise WcbError("Malformed", "salt is not 32 bytes")

    extensions = expect_array(descriptor["extensions"], "extensions")
    previous = None
    for index, item in enumerate(extensions):
        extension = expect_map(item, {"name", "value"}, "extension")
        name = expect_text(extension["name"], "extension.name")
        if not name:
            raise WcbError("Malformed", "extension name is empty")
        if previous is not None:
            if name == previous:
                raise WcbError("Malformed", "duplicate extension name")
            if name.encode("utf-8") < previous.encode("utf-8"):
                raise WcbError("Malformed", "extensions are unsorted")
        previous = name

    payload_hash = check_root(descriptor["payload_hash"], "payload_hash")
    if payload_hash != frame["content_hash"]:
        raise WcbError("Corrupt", "payload_hash disagrees with the frame content_hash")
    if expect_u64(page["payload_len"], "page.payload_len") != len(payload):
        raise WcbError("Malformed", "stored payload is shorter or longer than payload_len")
    if frame["decoded_len"] != page_bytes:
        raise WcbError("Corrupt", "frame decoded_len disagrees with page_bytes")

    return {
        "descriptor_root": hash_domain(DOMAIN_KV_PROV, descriptor_raw),
        "schema": "weft.ai.kv_prov.v1",
        "page_bytes": page_bytes,
        "payload_len": len(payload),
    }


# ---------------------------------------------------------------------------
# Registry


SCHEMA_VALIDATORS = {
    "weft.selector.v1": validate_selector,
    "weft.context-root.v1": validate_context_root,
    "weft.causal.v1": validate_causal,
    "weft.tensor.logical.v1": validate_logical_tensor,
    "weft.tensor.layout.v1": validate_tensor_layout,
    "weft.tensor.semantic.v1": validate_semantic_root,
    "weft.provenance.v1": validate_provenance,
    "weft.capability.v1": validate_capability_v1,
    "weft.capability.envelope.v1": validate_capability_envelope,
    "weft.ai.kv_prov.v1": validate_kv_prov_frame,
}
