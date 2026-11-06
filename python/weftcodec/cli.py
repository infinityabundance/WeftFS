"""Command-line reader for WFT1 frames."""

from __future__ import annotations

import argparse
import json
import sys

from . import modes
from .dependency import DirectoryFrameSourceResolver, DirectoryResolver
from .errors import WeftError
from .frame import parse
from .identity import verify_frame
from .limits import Limits

EXIT_OK = 0
EXIT_FAILURE = 1
EXIT_USAGE = 2


def _resolver(args):
    if getattr(args, "resolve", False):
        source = getattr(args, "frame_source", None)
        if not source:
            raise SystemExit("--resolve requires --frame-source")
        return DirectoryFrameSourceResolver(source, Limits())
    if getattr(args, "deps", None):
        return DirectoryResolver(args.deps)
    return None


def _base_frame(args):
    path = getattr(args, "base", None)
    if not path:
        return None
    with open(path, "rb") as handle:
        return handle.read()


def _print_error(exc: WeftError) -> None:
    print("%s: %s" % (exc.error_class, exc.message), file=sys.stderr)


def _verify(args) -> int:
    result = verify_frame(
        args.file, _resolver(args), Limits(), _base_frame(args)
    )
    if result.ok:
        if args.json:
            print(json.dumps(result.to_dict(), sort_keys=True))
        else:
            print(
                "OK mode=%s decoded_len=%d content_hash=%s chunks=%d dependencies=%d"
                % (
                    result.mode,
                    result.decoded_len,
                    result.content_hash,
                    result.chunk_count,
                    result.dependency_count,
                )
            )
        return EXIT_OK
    if args.json:
        print(json.dumps(result.to_dict(), sort_keys=True))
    else:
        print("%s: %s" % (result.error_class, result.error), file=sys.stderr)
    return EXIT_FAILURE


def _decode(args) -> int:
    try:
        with open(args.file, "rb") as handle:
            data = handle.read()
    except OSError as exc:
        print("Io: %s" % exc, file=sys.stderr)
        return EXIT_FAILURE
    try:
        plaintext = modes.decode_bytes(
            data, _resolver(args), Limits(), _base_frame(args)
        )
    except WeftError as exc:
        _print_error(exc)
        return EXIT_FAILURE
    if args.output == "-":
        sys.stdout.buffer.write(plaintext)
        sys.stdout.buffer.flush()
        return EXIT_OK
    try:
        with open(args.output, "wb") as handle:
            handle.write(plaintext)
    except OSError as exc:
        print("Io: %s" % exc, file=sys.stderr)
        return EXIT_FAILURE
    return EXIT_OK


def _inspect_record(frame) -> dict:
    header = frame.header
    record = {
        "ok": True,
        "mode": header.mode_name,
        "mode_value": header.mode,
        "format_major": header.format_major,
        "format_minor": header.format_minor,
        "domain": header.domain,
        "domain_name": "payload" if header.domain == 0 else "structured",
        "flags": header.flag_names(),
        "flags_value": header.flags,
        "features": header.feature_names(),
        "features_value": header.features,
        "level": header.level,
        "decoded_len": header.decoded_len,
        "body_len": header.body_len,
        "table_len": header.table_len,
        "frame_len": len(frame.raw),
        "chunk_count": header.chunk_count,
        "dep_count": header.dep_count,
        "dependency_count": header.dep_count,
        "content_hash": header.content_hash.hex(),
        "dependencies": [
            {
                "hash": dep.hash.hex(),
                "decoded_len": dep.decoded_len,
                "dep_domain": dep.dep_domain,
                "dep_flags": dep.dep_flags,
                "codec_hint": dep.codec_hint,
            }
            for dep in frame.dependencies
        ],
        "chunks": [
            {
                "encoded_off": chunk.encoded_off,
                "encoded_len": chunk.encoded_len,
                "decoded_len": chunk.decoded_len,
                "hash": chunk.hash.hex(),
            }
            for chunk in frame.chunks
        ],
    }
    limits = Limits()
    try:
        if header.mode == 6:
            record["tensor"] = modes.tensor_metadata(frame, limits)
        elif header.mode == 7:
            record["tokens"] = modes.token_metadata(frame, limits)
        elif header.mode == 8:
            record["quantized_tensor"] = modes.quantized_tensor_metadata(frame, limits)
        elif header.mode == 5:
            from . import wcb

            record["structured"] = _jsonable(wcb.decode(frame.body, limits))
    except WeftError as exc:
        record["descriptor_error"] = {
            "error_class": exc.error_class,
            "error": exc.message,
        }
    return record


def _jsonable(value):
    from . import wcb

    if value is None or isinstance(value, (bool, str, int, float)):
        return value
    if isinstance(value, (bytes, bytearray)):
        return {"bytes_hex": bytes(value).hex()}
    if isinstance(value, wcb.U64):
        return {"u64": value.value}
    if isinstance(value, wcb.I64):
        return {"i64": value.value}
    if isinstance(value, wcb.Timestamp):
        return {"timestamp_nanos": value.nanos}
    if isinstance(value, list):
        return [_jsonable(item) for item in value]
    if isinstance(value, dict):
        return {key: _jsonable(item) for key, item in value.items()}
    return repr(value)


def _inspect(args) -> int:
    try:
        with open(args.file, "rb") as handle:
            data = handle.read()
    except OSError as exc:
        print(json.dumps({"ok": False, "error_class": "Io", "error": str(exc)}))
        return EXIT_FAILURE
    try:
        frame = parse(data, Limits())
    except WeftError as exc:
        print(
            json.dumps(
                {"ok": False, "error_class": exc.error_class, "error": exc.message},
                sort_keys=True,
            )
        )
        return EXIT_FAILURE
    print(json.dumps(_inspect_record(frame), sort_keys=True))
    return EXIT_OK


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m weftcodec",
        description="Independent Python reader for Weft WFT1 frames and WCB.",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    verify = sub.add_parser("verify", help="validate a frame and every hash")
    verify.add_argument("file")
    verify.add_argument("--deps", help="directory of dependency files named by hex")
    verify.add_argument(
        "--base", help="base frame for WEIGHT_DELTA/SEMANTIC_DELTA/SPARSE_DELTA"
    )
    verify.add_argument(
        "--resolve", action="store_true", help="resolve dependencies as frames"
    )
    verify.add_argument(
        "--frame-source", dest="frame_source", help="directory of frames named by hex"
    )
    verify.add_argument("--json", action="store_true", help="emit a JSON result")
    verify.set_defaults(func=_verify)

    decode = sub.add_parser("decode", help="decode a frame to plaintext")
    decode.add_argument("file")
    decode.add_argument("output", help="output path, or - for stdout")
    decode.add_argument("--deps", help="directory of dependency files named by hex")
    decode.add_argument(
        "--base", help="base frame for WEIGHT_DELTA/SEMANTIC_DELTA/SPARSE_DELTA"
    )
    decode.add_argument(
        "--resolve", action="store_true", help="resolve dependencies as frames"
    )
    decode.add_argument(
        "--frame-source", dest="frame_source", help="directory of frames named by hex"
    )
    decode.set_defaults(func=_decode)

    inspect = sub.add_parser("inspect", help="emit frame metadata as JSON")
    inspect.add_argument("file")
    inspect.add_argument("--json", action="store_true", help=argparse.SUPPRESS)
    inspect.set_defaults(func=_inspect)
    return parser


def main(argv=None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
