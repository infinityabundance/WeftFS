// T046 / P1-12 GPU court: exact reconstruction of a real WeftCodec mode-9 frame.
//
// The exponent plane is Huffman-coded (variable length, inherently serial on the
// symbol stream). This program therefore measures two things honestly:
//   1. the parallel part of mode-9 decode — the sign/mantissa plane join — on the
//      GPU, including H2D and D2H, against the same join on the CPU;
//   2. the floor: the copies alone with no kernel.
// It also verifies byte-exactness against the CPU decoder's output.
//
// It does NOT claim a full GPU Huffman decode: see the results document.

#include <cuda_runtime.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <chrono>
#include <vector>
#include <string>
#include <algorithm>
#include <cstdint>

#define CK(x) do { cudaError_t e = (x); if (e != cudaSuccess) { \
  fprintf(stderr, "CUDA error %s at %s:%d\n", cudaGetErrorString(e), __FILE__, __LINE__); exit(2);} } while (0)

static std::vector<unsigned char> slurp(const std::string& path) {
  FILE* f = fopen(path.c_str(), "rb");
  if (!f) { fprintf(stderr, "cannot open %s\n", path.c_str()); exit(2); }
  fseek(f, 0, SEEK_END);
  long n = ftell(f);
  fseek(f, 0, SEEK_SET);
  std::vector<unsigned char> v((size_t)n);
  if (n > 0 && fread(v.data(), 1, (size_t)n, f) != (size_t)n) { fprintf(stderr, "short read\n"); exit(2); }
  fclose(f);
  return v;
}

static uint64_t fnv1a(const unsigned char* p, size_t n) {
  uint64_t h = 1469598103934665603ull;
  for (size_t i = 0; i < n; ++i) { h ^= p[i]; h *= 1099511628211ull; }
  return h;
}

// ---------------------------------------------------------------- Huffman ---
struct Huffman {
  int max_bits = 0;
  uint32_t count[33] = {0};
  uint32_t first_code[33] = {0};
  uint32_t first_index[33] = {0};
  std::vector<unsigned char> sorted;

  void build(const unsigned char* lengths) {
    for (int i = 0; i < 256; ++i) if (lengths[i]) { count[lengths[i]]++; max_bits = std::max(max_bits, (int)lengths[i]); }
    uint32_t code = 0, index = 0;
    for (int bits = 1; bits <= max_bits; ++bits) {
      code = (code + count[bits - 1]) << 1;
      first_code[bits] = code;
      first_index[bits] = index;
      index += count[bits];
    }
    sorted.assign(index, 0);
    std::vector<uint32_t> next(first_index, first_index + max_bits + 1);
    for (int s = 0; s < 256; ++s) {
      int len = lengths[s];
      if (len) sorted[next[len]++] = (unsigned char)s;
    }
  }

  // Serial MSB-first decode. Returns symbols written.
  size_t decode(const unsigned char* stream, size_t stream_len, unsigned char* out, size_t out_len) const {
    size_t bit = 0, written = 0;
    const size_t total_bits = stream_len * 8;
    while (written < out_len && bit < total_bits) {
      uint32_t code = 0;
      int len = 0;
      int found = -1;
      while (bit < total_bits && len < max_bits) {
        size_t byte = bit >> 3;
        int shift = 7 - (int)(bit & 7);
        code = (code << 1) | ((stream[byte] >> shift) & 1u);
        ++bit; ++len;
        if (count[len]) {
          uint32_t delta = code - first_code[len];
          if (delta < count[len]) { found = (int)(first_index[len] + delta); break; }
        }
      }
      if (found < 0) break;
      out[written++] = sorted[(size_t)found];
    }
    return written;
  }
};

// ------------------------------------------------------------------ kernels --
// BF16: mantissa byte holds (sign << 7) | (mantissa7); exponent is a full byte.
__global__ void join_bf16(const unsigned char* __restrict__ exp,
                          const unsigned char* __restrict__ mant,
                          unsigned char* __restrict__ out,
                          size_t n) {
  size_t i = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  size_t stride = (size_t)gridDim.x * blockDim.x;
  for (; i < n; i += stride) {
    unsigned m = mant[i];
    unsigned e = exp[i];
    unsigned word = ((m >> 7) << 15) | (e << 7) | (m & 0x7fu);
    out[2 * i]     = (unsigned char)(word & 0xffu);
    out[2 * i + 1] = (unsigned char)((word >> 8) & 0xffu);
  }
}

__global__ void join_bf16_vec4(const unsigned char* __restrict__ exp,
                               const unsigned char* __restrict__ mant,
                               unsigned char* __restrict__ out,
                               size_t n) {
  size_t base = ((size_t)blockIdx.x * blockDim.x + threadIdx.x) * 4;
  size_t stride = (size_t)gridDim.x * blockDim.x * 4;
  for (; base + 3 < n; base += stride) {
    uchar4 m = *reinterpret_cast<const uchar4*>(mant + base);
    uchar4 e = *reinterpret_cast<const uchar4*>(exp + base);
    unsigned char mb[4] = {m.x, m.y, m.z, m.w};
    unsigned char eb[4] = {e.x, e.y, e.z, e.w};
    unsigned char ob[8];
    #pragma unroll
    for (int k = 0; k < 4; ++k) {
      unsigned mm = mb[k], ee = eb[k];
      unsigned w = ((mm >> 7) << 15) | (ee << 7) | (mm & 0x7fu);
      ob[2 * k] = (unsigned char)(w & 0xffu);
      ob[2 * k + 1] = (unsigned char)((w >> 8) & 0xffu);
    }
    memcpy(out + 2 * base, ob, 8);
  }
}

// Fused decode-and-consume: the reconstructed words never leave the device.
// The consumer here is an order-independent XOR fingerprint over splitmix64(word),
// which the CPU can recompute to prove the device saw the same values.
__device__ __forceinline__ unsigned long long splitmix64(unsigned long long x) {
  x += 0x9E3779B97F4A7C15ull;
  x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ull;
  x = (x ^ (x >> 27)) * 0x94D049BB133111EBull;
  return x ^ (x >> 31);
}

__global__ void join_hash_bf16(const unsigned char* __restrict__ exp,
                               const unsigned char* __restrict__ mant,
                               unsigned long long* __restrict__ acc,
                               size_t n) {
  size_t i = (size_t)blockIdx.x * blockDim.x + threadIdx.x;
  size_t stride = (size_t)gridDim.x * blockDim.x;
  unsigned long long local = 0;
  for (; i < n; i += stride) {
    unsigned m = mant[i], e = exp[i];
    unsigned word = ((m >> 7) << 15) | (e << 7) | (m & 0x7fu);
    local ^= splitmix64((unsigned long long)word);
  }
  atomicXor(acc, local);
}

// --------------------------------------------------------------------- main --
int main(int argc, char** argv) {
  std::string dir = argc > 1 ? argv[1] : ".";
  int reps = argc > 2 ? atoi(argv[2]) : 7;

  auto body = slurp(dir + "/body.bin");
  auto plain = slurp(dir + "/plain.bin");
  auto meta_txt = slurp(dir + "/meta.txt");
  size_t elements = 0;
  { std::string s(meta_txt.begin(), meta_txt.end()); size_t p = s.find("elements="); elements = strtoull(s.c_str() + p + 9, nullptr, 10); }

  uint32_t desc_len; memcpy(&desc_len, body.data(), 4);
  size_t off = 4 + desc_len;
  size_t mant_len = elements;              // BF16: one byte per element
  size_t exp_len = body.size() - off - mant_len;

  const unsigned char* exp_plane = body.data() + off;
  const unsigned char* mant = exp_plane + exp_len;

  Huffman h; h.build(exp_plane);
  std::vector<unsigned char> exp(elements);

  auto t0 = std::chrono::steady_clock::now();
  size_t got = h.decode(exp_plane + 256, exp_len - 256, exp.data(), elements);
  auto t1 = std::chrono::steady_clock::now();
  double cpu_huff_ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
  if (got != elements) { fprintf(stderr, "huffman decoded %zu of %zu symbols\n", got, elements); return 3; }

  // CPU join (the parallel part, measured alone).
  std::vector<unsigned char> cpu_out(elements * 2);
  auto t2 = std::chrono::steady_clock::now();
  for (size_t i = 0; i < elements; ++i) {
    unsigned m = mant[i], e = exp[i];
    unsigned w = ((m >> 7) << 15) | (e << 7) | (m & 0x7fu);
    cpu_out[2 * i] = (unsigned char)(w & 0xffu);
    cpu_out[2 * i + 1] = (unsigned char)((w >> 8) & 0xffu);
  }
  auto t3 = std::chrono::steady_clock::now();
  double cpu_join_ms = std::chrono::duration<double, std::milli>(t3 - t2).count();

  if (memcmp(cpu_out.data(), plain.data(), plain.size()) != 0) { fprintf(stderr, "CPU reconstruction != plaintext\n"); return 4; }

  // ---- GPU, pageable staging ----
  unsigned char *d_exp, *d_mant, *d_out;
  CK(cudaMalloc(&d_exp, elements));
  CK(cudaMalloc(&d_mant, mant_len));
  CK(cudaMalloc(&d_out, elements * 2));
  std::vector<unsigned char> gpu_out(elements * 2);

  const int threads = 256;
  int blocks = (int)((elements + threads * 4 - 1) / (threads * 4));
  if (blocks > 65535) blocks = 65535;

  double pageable_total[16], pageable_kernel[16], pageable_copy[16];
  for (int r = 0; r < reps; ++r) {
    cudaEvent_t a, b, c; CK(cudaEventCreate(&a)); CK(cudaEventCreate(&b)); CK(cudaEventCreate(&c));
    auto w0 = std::chrono::steady_clock::now();
    CK(cudaMemcpy(d_exp, exp.data(), elements, cudaMemcpyHostToDevice));
    CK(cudaMemcpy(d_mant, mant, mant_len, cudaMemcpyHostToDevice));
    CK(cudaEventRecord(a));
    join_bf16_vec4<<<blocks, threads>>>(d_exp, d_mant, d_out, elements);
    CK(cudaEventRecord(b));
    CK(cudaMemcpy(gpu_out.data(), d_out, elements * 2, cudaMemcpyDeviceToHost));
    CK(cudaEventSynchronize(b));
    auto w1 = std::chrono::steady_clock::now();
    float kms = 0; CK(cudaEventElapsedTime(&kms, a, b));
    pageable_kernel[r] = kms;
    pageable_total[r] = std::chrono::duration<double, std::milli>(w1 - w0).count();
    pageable_copy[r] = pageable_total[r] - kms;
    if (memcmp(gpu_out.data(), plain.data(), plain.size()) != 0) {
      size_t bad = 0; while (bad < plain.size() && gpu_out[bad] == plain[bad]) ++bad;
      size_t e = bad / 2;
      fprintf(stderr, "GPU reconstruction != plaintext on rep %d at byte %zu (element %zu): plain=%02x gpu=%02x exp=%02x mant=%02x\n",
              r, bad, e, plain[bad], gpu_out[bad], (unsigned)exp[e], (unsigned)mant[e]);
      return 5;
    }
    CK(cudaEventDestroy(a)); CK(cudaEventDestroy(b)); CK(cudaEventDestroy(c));
  }

  // ---- GPU, pinned staging ----
  unsigned char *h_exp, *h_mant, *h_out;
  CK(cudaMallocHost(&h_exp, elements));
  CK(cudaMallocHost(&h_mant, mant_len));
  CK(cudaMallocHost(&h_out, elements * 2));
  memcpy(h_exp, exp.data(), elements);
  memcpy(h_mant, mant, mant_len);

  double pinned_total[16], pinned_kernel[16];
  for (int r = 0; r < reps; ++r) {
    cudaEvent_t a, b; CK(cudaEventCreate(&a)); CK(cudaEventCreate(&b));
    auto w0 = std::chrono::steady_clock::now();
    CK(cudaMemcpy(d_exp, h_exp, elements, cudaMemcpyHostToDevice));
    CK(cudaMemcpy(d_mant, h_mant, mant_len, cudaMemcpyHostToDevice));
    CK(cudaEventRecord(a));
    join_bf16_vec4<<<blocks, threads>>>(d_exp, d_mant, d_out, elements);
    CK(cudaEventRecord(b));
    CK(cudaMemcpy(h_out, d_out, elements * 2, cudaMemcpyDeviceToHost));
    CK(cudaEventSynchronize(b));
    auto w1 = std::chrono::steady_clock::now();
    float kms = 0; CK(cudaEventElapsedTime(&kms, a, b));
    pinned_kernel[r] = kms;
    pinned_total[r] = std::chrono::duration<double, std::milli>(w1 - w0).count();
    if (memcmp(h_out, plain.data(), plain.size()) != 0) { fprintf(stderr, "pinned GPU reconstruction != plaintext\n"); return 6; }
    CK(cudaEventDestroy(a)); CK(cudaEventDestroy(b));
  }

  auto med = [](double* v, int n) { std::vector<double> x(v, v + n); std::sort(x.begin(), x.end()); return n % 2 ? x[n / 2] : 0.5 * (x[n / 2 - 1] + x[n / 2]); };

  // ---- fused device-resident consumer ----
  unsigned long long* d_acc;
  CK(cudaMalloc(&d_acc, sizeof(unsigned long long)));
  double fused_total[16];
  unsigned long long fused_value = 0;
  for (int r = 0; r < reps; ++r) {
    CK(cudaMemset(d_acc, 0, sizeof(unsigned long long)));
    auto w0 = std::chrono::steady_clock::now();
    CK(cudaMemcpy(d_exp, h_exp, elements, cudaMemcpyHostToDevice));
    CK(cudaMemcpy(d_mant, h_mant, mant_len, cudaMemcpyHostToDevice));
    join_hash_bf16<<<blocks, threads>>>(d_exp, d_mant, d_acc, elements);
    CK(cudaMemcpy(&fused_value, d_acc, sizeof(unsigned long long), cudaMemcpyDeviceToHost));
    auto w1 = std::chrono::steady_clock::now();
    fused_total[r] = std::chrono::duration<double, std::milli>(w1 - w0).count();
  }
  unsigned long long cpu_fused = 0;
  for (size_t i = 0; i < elements; ++i) {
    unsigned m = mant[i], e = exp[i];
    unsigned w = ((m >> 7) << 15) | (e << 7) | (m & 0x7fu);
    unsigned long long x = (unsigned long long)w + 0x9E3779B97F4A7C15ull;
    x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ull;
    x = (x ^ (x >> 27)) * 0x94D049BB133111EBull;
    cpu_fused ^= x ^ (x >> 31);
  }
  bool fused_exact = (cpu_fused == fused_value);

  printf("{\n");
  printf("  \"elements\": %zu,\n", elements);
  printf("  \"exp_plane_bytes\": %zu,\n", exp_len);
  printf("  \"mant_plane_bytes\": %zu,\n", mant_len);
  printf("  \"output_bytes\": %zu,\n", elements * 2);
  printf("  \"cpu_huffman_decode_ms\": %.3f,\n", cpu_huff_ms);
  printf("  \"cpu_join_ms\": %.3f,\n", cpu_join_ms);
  printf("  \"gpu_pageable_total_ms\": %.3f,\n", med(pageable_total, reps));
  printf("  \"gpu_pageable_kernel_ms\": %.3f,\n", med(pageable_kernel, reps));
  printf("  \"gpu_pageable_copy_ms\": %.3f,\n", med(pageable_copy, reps));
  printf("  \"gpu_pinned_total_ms\": %.3f,\n", med(pinned_total, reps));
  printf("  \"gpu_pinned_kernel_ms\": %.3f,\n", med(pinned_kernel, reps));
  printf("  \"fused_device_only_total_ms\": %.3f,\n", med(fused_total, reps));
  printf("  \"fused_exact\": %s,\n", fused_exact ? "true" : "false");
  printf("  \"reps\": %d,\n", reps);
  printf("  \"exact_plaintext\": true,\n");
  printf("  \"plaintext_fnv1a\": \"%016llx\",\n", (unsigned long long)fnv1a(plain.data(), plain.size()));
  printf("  \"reconstructed_fnv1a\": \"%016llx\"\n", (unsigned long long)fnv1a(gpu_out.data(), gpu_out.size()));
  printf("}\n");

  CK(cudaFree(d_exp)); CK(cudaFree(d_mant)); CK(cudaFree(d_out)); CK(cudaFree(d_acc));
  CK(cudaFreeHost(h_exp)); CK(cudaFreeHost(h_mant)); CK(cudaFreeHost(h_out));
  return 0;
}
