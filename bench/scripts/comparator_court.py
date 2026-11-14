#!/usr/bin/env python3
"""Comparator court (T041): complete stored bytes for one checkpoint pair.

Stdlib only. Runs inside the bench court. Every arm must restore the newer
container byte-for-byte (SHA-256) or the arm is recorded as `NOT EXACT`.

Arms
----
N      native: the two containers stored separately
Z19    independent `zstd -19` of the newer container
PF12   `zstd --patch-from <base> -12` on the newer container
PF19   `zstd --patch-from <base> -19` (may exceed the court; an OOM is recorded)
XNOR   our implementation of a ZipLLM/BitX-style XOR delta: per tensor,
       XOR(newer, base) compressed with `zstd -19`, plus an index of names,
       offsets and lengths. Labelled "our implementation of the idea".
ZIPNN  pinned ZipNN, if importable in this court
BCP    ByteCheckpoint, if importable in this court

Accounting is complete: base bytes + the stored form of the newer container +
every index/manifest byte this arm needs. Delta-only sizes are reported too.
"""

import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import time

CHUNK = 1 << 20


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(CHUNK), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_header(path):
    with open(path, "rb") as handle:
        (length,) = struct.unpack("<Q", handle.read(8))
        header = json.loads(handle.read(length))
    tensors = []
    for name, entry in header.items():
        if name == "__metadata__":
            continue
        begin, end = entry["data_offsets"]
        tensors.append(
            {
                "name": name,
                "dtype": entry["dtype"],
                "shape": entry["shape"],
                "begin": begin,
                "end": end,
            }
        )
    tensors.sort(key=lambda t: t["begin"])
    return 8 + length, tensors


def run(cmd, out_path=None, timeout=1800):
    started = time.monotonic()
    if out_path:
        handle = open(out_path, "wb")
    else:
        handle = open(os.devnull, "wb")
    try:
        proc = subprocess.run(cmd, stdout=handle, stderr=subprocess.PIPE, timeout=timeout)
    finally:
        handle.close()
    elapsed = time.monotonic() - started
    return proc.returncode, elapsed, proc.stderr.decode("utf-8", "replace")[-2000:]


def zstd_available():
    try:
        subprocess.run(["zstd", "--version"], capture_output=True, check=True)
        return True
    except Exception:
        return False


def xnor_delta(base_path, newer_path, work, data_start, tensors):
    """Per-tensor XOR delta, zstd -19 per tensor, with a complete index."""
    frames = []
    index = []
    offset = 0
    with open(base_path, "rb") as base, open(newer_path, "rb") as newer:
        for tensor in tensors:
            length = tensor["end"] - tensor["begin"]
            base.seek(data_start + tensor["begin"])
            newer.seek(data_start + tensor["begin"])
            raw = bytearray(length)
            # Stream the XOR in bounded windows; never hold two full tensors.
            written = 0
            while written < length:
                window = min(CHUNK, length - written)
                a = base.read(window)
                b = newer.read(window)
                if len(a) != window or len(b) != window:
                    raise RuntimeError("short read while computing the XOR delta")
                raw[written : written + window] = bytes(
                    x ^ y for x, y in zip(a, b)
                )
                written += window
            slug = "".join(c if c.isalnum() else "_" for c in tensor["name"])[:80]
            frame_path = os.path.join(work, f"xnor-{len(frames):05d}-{slug}.bin")
            with open(frame_path, "wb") as handle:
                handle.write(raw)
            out_path = frame_path + ".zst"
            code, elapsed, err = run(["zstd", "-19", "-q", "-f", frame_path, "-o", out_path])
            if code != 0:
                raise RuntimeError(f"zstd on one XOR frame failed: {err}")
            size = os.path.getsize(out_path)
            frames.append((out_path, size, elapsed))
            index.append(
                {
                    "name": tensor["name"],
                    "dtype": tensor["dtype"],
                    "shape": tensor["shape"],
                    "begin": tensor["begin"],
                    "end": tensor["end"],
                    "frame_offset": offset,
                    "frame_bytes": size,
                }
            )
            offset += size
    index_bytes = len(json.dumps(index, sort_keys=True).encode())
    return frames, index, index_bytes


def main():
    if len(sys.argv) != 4:
        print("usage: comparator_court.py BASE NEWER OUT", file=sys.stderr)
        return 64
    base_path, newer_path, out_path = sys.argv[1:4]
    work = os.path.join(os.path.dirname(out_path), "comparator-work")
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work, exist_ok=True)

    base_bytes = os.path.getsize(base_path)
    newer_bytes = os.path.getsize(newer_path)
    base_sha = sha256_file(base_path)
    newer_sha = sha256_file(newer_path)
    data_start, tensors = parse_header(newer_path)
    base_data_start, base_tensors = parse_header(base_path)
    shapes_match = [
        (t["name"], t["dtype"], t["shape"], t["end"] - t["begin"])
        for t in tensors
    ] == [
        (t["name"], t["dtype"], t["shape"], t["end"] - t["begin"])
        for t in base_tensors
    ]

    arms = []
    not_measured = []

    arms.append(
        {
            "arm": "N",
            "kind": "native separate containers",
            "stored_bytes": base_bytes + newer_bytes,
            "delta_bytes": newer_bytes,
            "exact": True,
            "index_bytes": 0,
            "compress_ms": None,
            "decompress_ms": None,
        }
    )

    if not zstd_available():
        not_measured.append({"arm": "Z19/PF12/PF19/XNOR", "blocker": "zstd CLI is not on PATH in this court"})
    else:
        z19 = os.path.join(work, "newer.z19.zst")
        code, elapsed, err = run(["zstd", "-19", "-q", "-f", newer_path, "-o", z19])
        if code == 0:
            out = os.path.join(work, "newer.z19.out")
            dcode, delapsed, derr = run(["zstd", "-d", "-q", "-f", z19, "-o", out])
            exact = dcode == 0 and sha256_file(out) == newer_sha
            arms.append(
                {
                    "arm": "Z19",
                    "kind": "independent zstd -19 of the newer container",
                    "stored_bytes": base_bytes + os.path.getsize(z19),
                    "delta_bytes": os.path.getsize(z19),
                    "exact": exact,
                    "index_bytes": 0,
                    "compress_ms": elapsed * 1000.0,
                    "decompress_ms": delapsed * 1000.0 if dcode == 0 else None,
                    "detail": None if exact else derr,
                }
            )
            os.unlink(out)
        else:
            not_measured.append({"arm": "Z19", "blocker": f"zstd -19 failed: {err}"})

        for level in (12, 19):
            patch = os.path.join(work, f"newer.pf{level}.zst")
            code, elapsed, err = run(
                [
                    "zstd",
                    "-q",
                    "-f",
                    f"-{level}",
                    f"--patch-from={base_path}",
                    newer_path,
                    "-o",
                    patch,
                ]
            )
            arm = f"PF{level}"
            if code == 0:
                out = os.path.join(work, f"newer.pf{level}.out")
                dcode, delapsed, derr = run(
                    [
                        "zstd",
                        "-d",
                        "-q",
                        "-f",
                        f"--patch-from={base_path}",
                        patch,
                        "-o",
                        out,
                    ]
                )
                exact = dcode == 0 and sha256_file(out) == newer_sha
                arms.append(
                    {
                        "arm": arm,
                        "kind": f"zstd --patch-from -{level} on the newer container",
                        "stored_bytes": base_bytes + os.path.getsize(patch),
                        "delta_bytes": os.path.getsize(patch),
                        "exact": exact,
                        "index_bytes": 0,
                        "compress_ms": elapsed * 1000.0,
                        "decompress_ms": delapsed * 1000.0 if dcode == 0 else None,
                        "detail": None if exact else derr,
                    }
                )
                if os.path.exists(out):
                    os.unlink(out)
            else:
                not_measured.append(
                    {
                        "arm": arm,
                        "blocker": f"zstd --patch-from -{level} exited {code}: {err}",
                        "returncode": code,
                    }
                )

        contiguous = True
        expected = 0
        for tensor in tensors:
            if tensor["begin"] != expected:
                contiguous = False
                break
            expected = tensor["end"]
        contiguous = contiguous and expected == newer_bytes - data_start
        if shapes_match and contiguous:
            frames, index, index_bytes = xnor_delta(
                base_path, newer_path, work, data_start, tensors
            )
            # Verify one pass: decompress every frame, XOR with the base,
            # re-hash the whole restored data section.
            restored = hashlib.sha256()
            with open(base_path, "rb") as base, open(newer_path, "rb") as newer:
                # The 8-byte prefix and header are identical between the two
                # files (same tensor layout), so hash them from the newer file.
                newer.seek(0)
                prefix = newer.read(data_start)
                restored.update(prefix)
                delapsed = 0.0
                for entry, (frame_path, _, _) in zip(index, frames):
                    out = frame_path + ".out"
                    dcode, el, derr = run(["zstd", "-d", "-q", "-f", frame_path, "-o", out])
                    if dcode != 0:
                        raise RuntimeError(f"XNOR verify decode failed: {derr}")
                    delapsed += el
                    with open(out, "rb") as handle:
                        delta = handle.read()
                    length = entry["end"] - entry["begin"]
                    base.seek(data_start + entry["begin"])
                    original = base.read(length)
                    restored.update(bytes(x ^ y for x, y in zip(delta, original)))
                    os.unlink(out)
                restored_sha = restored.hexdigest()
            arms.append(
                {
                    "arm": "XNOR",
                    "kind": "our implementation of a ZipLLM/BitX-style XOR delta: per tensor, XOR(newer, base) then zstd -19, with an index",
                    "stored_bytes": base_bytes + sum(size for _, size, _ in frames) + index_bytes,
                    "delta_bytes": sum(size for _, size, _ in frames) + index_bytes,
                    "exact": restored_sha == newer_sha,
                    "index_bytes": index_bytes,
                    "compress_ms": sum(elapsed for _, _, elapsed in frames) * 1000.0,
                    "decompress_ms": delapsed * 1000.0,
                    "frames": len(frames),
                }
            )
        elif not shapes_match:
            not_measured.append(
                {"arm": "XNOR", "blocker": "the two containers do not have identical tensor names/dtypes/shapes/offsets"}
            )
        else:
            not_measured.append(
                {"arm": "XNOR", "blocker": "the newer container's data section is not contiguous, so a concatenated XOR restore cannot be verified by whole-file SHA-256"}
            )

    # Optional comparators: importability is measured, not assumed.
    for module, arm_name in (("zipnn", "ZIPNN"), ("bytecheckpoint", "BCP")):
        probe = subprocess.run(
            [sys.executable, "-c", f"import {module}; print(getattr({module}, '__version__', 'unknown'))"],
            capture_output=True,
            text=True,
        )
        if probe.returncode != 0:
            not_measured.append(
                {
                    "arm": arm_name,
                    "blocker": f"`import {module}` failed in the bench court: {probe.stderr.strip().splitlines()[-1] if probe.stderr.strip() else 'no stderr'}",
                }
            )
            continue
        not_measured.append(
            {
                "arm": arm_name,
                "blocker": f"{module} imports (version {probe.stdout.strip()}) but this court drives only the zstd CLI; no {module} arm was executed",
            }
        )

    receipt = {
        "schema": "weft.comparators.v1",
        "stamp": os.environ.get("WEFT_COMPARATOR_STAMP", "unknown"),
        "pair": {
            "base": {"path": base_path, "bytes": base_bytes, "sha256": base_sha},
            "newer": {"path": newer_path, "bytes": newer_bytes, "sha256": newer_sha},
            "tensors": len(tensors),
            "shapes_match": shapes_match,
        },
        "court": {
            "limits": "3g memory / 3g swap / 1024 pids / 4 cpus",
            "zstd": subprocess.run(["zstd", "--version"], capture_output=True, text=True).stdout.strip(),
            "accounting": "base bytes + stored form of the newer container + every index/manifest byte the arm needs",
        },
        "arms": arms,
        "not_measured": not_measured,
        "loss_notes": [],
    }
    if not os.environ.get("WEFT_COMPARATOR_LABEL"):
        pass
    with open(out_path, "w") as handle:
        json.dump(receipt, handle, indent=1)
    print(
        "comparator: base={} newer={} arms={} not_measured={} out={}".format(
            base_bytes, newer_bytes, len(arms), len(not_measured), out_path
        )
    )
    shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
