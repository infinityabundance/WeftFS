#!/usr/bin/env python3
"""Probe whether the DFloat11 lossless baseline can run in the court.

The package is imported (not imported lazily), so a CUDA device is required for
any use. This script records the exact import-time outcome and the CUDA driver
status rather than guessing. It exits 0 whether or not the baseline is
available; the receipt says which.

Usage:
  PYTHONPATH=<pylibs> python3 bench/scripts/dfloat11_probe.py --out result.json
"""

import argparse
import glob
import json
import platform
import sys
import traceback


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    result = {
        "schema": "weft.real-model.baseline.dfloat11-probe.v1",
        "baseline": "DFloat11",
        "python": sys.version,
        "platform": platform.platform(),
        "nvidia_device_nodes": glob.glob("/dev/nvidia*"),
        "available": False,
    }
    try:
        import importlib.metadata

        result["version"] = importlib.metadata.version("dfloat11")
    except Exception as err:  # noqa: BLE001 - recorded verbatim
        result["version_error"] = f"{type(err).__name__}: {err}"

    try:
        import cupy

        result["cupy_version"] = cupy.__version__
        result["cuda_device_count"] = int(cupy.cuda.runtime.getDeviceCount())
        result["cuda_runtime_version"] = int(cupy.cuda.runtime.runtimeGetVersion())
        result["cuda_driver_version"] = int(cupy.cuda.runtime.driverGetVersion())
    except Exception as err:  # noqa: BLE001
        result["cuda_probe_error"] = f"{type(err).__name__}: {err}"
        result["cuda_probe_traceback_tail"] = traceback.format_exc().splitlines()[-4:]

    try:
        import dfloat11  # noqa: F401

        result["import_ok"] = True
        result["exported"] = [name for name in dir(dfloat11) if not name.startswith("_")]
        # A tiny compression trial: a 4x4 bf16 module.
        import torch

        class Tiny(torch.nn.Module):
            def __init__(self):
                super().__init__()
                self.weight = torch.nn.Parameter(
                    torch.randn(4, 4, dtype=torch.bfloat16)
                )

        try:
            from dfloat11 import compress_model

            compress_model(
                Tiny(),
                {"": ["weight"]},
                "/tmp/dfloat11-trial",
                check_correctness=True,
            )
            result["compress_ok"] = True
        except Exception as err:  # noqa: BLE001
            result["compress_error"] = f"{type(err).__name__}: {err}"
            result["compress_traceback_tail"] = traceback.format_exc().splitlines()[-5:]
    except Exception as err:  # noqa: BLE001
        result["import_ok"] = False
        result["import_error"] = f"{type(err).__name__}: {err}"
        result["import_traceback_tail"] = traceback.format_exc().splitlines()[-6:]

    with open(args.out, "w") as handle:
        json.dump(result, handle, indent=2)
        handle.write("\n")
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
