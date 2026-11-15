# bench/gpu — the CUDA courts

These are the only courts in the repository that run **on the host device**
rather than inside `scripts/docker-court.sh`. That is deliberate and documented:
CUDA requires the GPU (AGENTS.md directive 1, guardrail 7). Everything else
still runs in a court.

## `mode9_gpu.cu`

Exact reconstruction of a real `mode 9` (`FLOAT_TENSOR`) BF16 frame.

* `bench/gpu/prep` is a harness crate that uses the **shipped** encoder to
  produce `frame.bin`, `body.bin`, `plain.bin` and `meta.txt` for a
  deterministic synthetic tensor. It also records the shipped decoder's own
  timing and asserts the decoder reproduces the source exactly.
* `mode9_gpu.cu` parses the frame body, rebuilds the canonical Huffman table
  from the plane's 256 length bytes, decodes the exponent plane serially on the
  CPU, and then measures three arms: CPU join, GPU join (pageable and pinned
  staging), and a fused device-resident consumer whose fingerprint is
  recomputed on the CPU to prove the device saw the same values.
* Every arm is compared byte-for-byte against the plaintext.

Run it with `scripts/gpu_mode9_court.sh`, which refuses (exit 78) when
`nvidia-smi`, `nvcc` or `/dev/nvidia0` is absent, so the aggregate can skip it
cleanly.

Read `bench/RESULTS-gpu-mode9.md` before citing any of it: the measured result
is a **loss** for GPU-resident decode at these sizes, because the serial Huffman
decode dominates and the copies cost more than the join saves.
