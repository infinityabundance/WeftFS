# GPU-resident exact reconstruction (T046 / P1-12) — measured, and it does not win

**Status:** measured on real hardware. **Result: a loss for GPU-resident decode of mode 9 at these sizes.**
Receipt: `bench/receipts/gpu-mode9-20261115T013050Z.json`. Court: `scripts/gpu_mode9_court.sh`.

This court is the documented exception to the Docker-only build rule (AGENTS.md
directive 1, guardrail 7): CUDA needs the device. Every other court still runs
inside `scripts/docker-court.sh`.

## What was measured

A real `mode 9` (`FLOAT_TENSOR`) BF16 frame, encoded by the **shipped** encoder,
at three sizes, on an NVIDIA GeForce RTX 4080 SUPER (sm_89, CUDA 13.4). Three
arms, seven repetitions each, medians recorded:

| arm | what it does |
|---|---|
| CPU join | the sign/mantissa plane join alone, single-threaded |
| GPU join | H2D exponent+mantissa, vec4 join kernel, D2H the reconstructed tensor |
| GPU fused | H2D the planes, join **and** consume on device; only a fingerprint returns |

The exponent plane is Huffman-coded and is decoded serially on the CPU in every
arm — that is not a shortcut, it is the structure of the format: the symbol
stream is variable-length and resynchronises only by decoding.

Byte-exactness is asserted, not asserted-about: the reconstructed tensor is
compared with the plaintext and with the shipped decoder's output, and the fused
arm's device-side fingerprint is recomputed on the CPU. All comparisons pass at
all three sizes; the FNV-1a digests are recorded in the receipt.

## Findings

1. **The serial Huffman decode dominates mode-9 decode** and is untouched by the
   GPU. Any "GPU decode" claim that omits it is measuring a fraction of the work.
2. **The parallel join is genuinely fast on the device** — the kernel alone is
   roughly an order of magnitude quicker than the single-threaded CPU join — but
   it is a small fraction of total decode time.
3. **Copies dominate at these sizes.** Moving the two planes in and the tensor
   out costs more than the join saves, so the GPU join *end to end* is slower
   than the CPU join. Pinned staging is dramatically better than pageable
   staging, and is the only configuration worth considering.
4. **The fused device-resident consumer does not rescue it** at these sizes: it
   avoids the D2H of the tensor but still pays the H2D of the planes, and the
   scalar fused kernel loses some of the vec4 join's throughput.
5. **No arithmetic claim is hidden here:** the device does no approximation, and
   every byte matches. The loss is a transfer and Amdahl loss, not an exactness
   loss.

## What this means for the plan

* `T046` is decided **negative at these sizes on this device**, with the
  mechanism proven correct. A future win would need the consumer to live on the
  device (a real kernel fused with the join) **and** the encoded planes to
  arrive without a host round trip, or a much larger per-call payload so the
  copy is amortised. Neither is demonstrated here.
* `P1-12` (nvCOMP GDeflate/LZ4/zstd paths) is **not measured**: nvCOMP is not
  installed on this host and the court records that rather than substituting a
  different comparator. A host with nvCOMP is the next step for that item, and
  it must be measured against the same frame bytes.
* `T047` (pinned staging) is answered in the small: pinned staging is the only
  viable host-buffer mode, and the receipt quantifies both. GPUDirect Storage
  remains unmeasured (no compatible physical storage on this host).

## Non-claims

One device, one host, one desktop GPU also driving a display; single-threaded
CPU comparison; no multi-GPU, no fleet, no cloud, no power or thermal analysis.
No claim that GPU decode is impossible elsewhere — only that it did not win here,
for the measured sizes, with the copies included.
