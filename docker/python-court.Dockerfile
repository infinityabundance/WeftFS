FROM python:3.12-slim

# Minimal Python court for the independent WeftCodec Python reader. Only the
# runtime cache library the Rust CLI needs (libgcc_s) and the test/build
# dependencies are installed; no compilers are needed because blake3 and
# zstandard publish manylinux wheels.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates libgcc-s1 \
 && rm -rf /var/lib/apt/lists/*

# PyTorch CPU is pinned so the DCP adapter tests run real
# `torch.distributed.checkpoint` against a known API. The CPU wheel is the
# official download.pytorch.org build (2.15.0+cpu, 209.5 MB as downloaded);
# no CUDA runtime is present and no GPU claim is made.
RUN pip install --no-cache-dir \
      "pytest>=8,<9" \
      "blake3>=0.3.1" \
      "zstandard>=0.22" \
      "build>=1.2" \
      "setuptools>=68" \
      wheel \
 && pip install --no-cache-dir --index-url https://download.pytorch.org/whl/cpu \
      "torch==2.15.0+cpu"

ENV PYTHONDONTWRITEBYTECODE=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONUNBUFFERED=1
WORKDIR /work
