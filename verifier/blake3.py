"""BLAKE3-256 in pure Python (standard library only).

This is a clean-room implementation written from the BLAKE3 specification and
the public reference algorithm. It exists so the independent verifier has no
dependency on the `blake3` PyPI package or on any WeftFS code. It is not
constant-time and is not intended for bulk data; the verifier hashes records
and payloads smaller than a few megabytes.

Only unkeyed hashing with a 32-byte output is used. The full tree construction
is implemented so inputs above 1024 bytes hash correctly.
"""

from __future__ import annotations

OUT_LEN = 32
BLOCK_LEN = 64
CHUNK_LEN = 1024

CHUNK_START = 1 << 0
CHUNK_END = 1 << 1
PARENT = 1 << 2
ROOT = 1 << 3

IV = (
    0x6A09E667,
    0xBB67AE85,
    0x3C6EF372,
    0xA54FF53A,
    0x510E527F,
    0x9B05688C,
    0x1F83D9AB,
    0x5BE0CD19,
)

MSG_PERMUTATION = (2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8)

_MASK = 0xFFFFFFFF


def _rotr(x: int, n: int) -> int:
    return ((x >> n) | (x << (32 - n))) & _MASK


def _g(state, a, b, c, d, mx, my):
    state[a] = (state[a] + state[b] + mx) & _MASK
    state[d] = _rotr(state[d] ^ state[a], 16)
    state[c] = (state[c] + state[d]) & _MASK
    state[b] = _rotr(state[b] ^ state[c], 12)
    state[a] = (state[a] + state[b] + my) & _MASK
    state[d] = _rotr(state[d] ^ state[a], 8)
    state[c] = (state[c] + state[d]) & _MASK
    state[b] = _rotr(state[b] ^ state[c], 7)


def _round(state, m):
    _g(state, 0, 4, 8, 12, m[0], m[1])
    _g(state, 1, 5, 9, 13, m[2], m[3])
    _g(state, 2, 6, 10, 14, m[4], m[5])
    _g(state, 3, 7, 11, 15, m[6], m[7])
    _g(state, 0, 5, 10, 15, m[8], m[9])
    _g(state, 1, 6, 11, 12, m[10], m[11])
    _g(state, 2, 7, 8, 13, m[12], m[13])
    _g(state, 3, 4, 9, 14, m[14], m[15])


def _compress(cv, block_words, counter, block_len, flags):
    state = [
        cv[0], cv[1], cv[2], cv[3], cv[4], cv[5], cv[6], cv[7],
        IV[0], IV[1], IV[2], IV[3],
        counter & _MASK, (counter >> 32) & _MASK,
        block_len, flags,
    ]
    block = list(block_words)
    for i in range(7):
        _round(state, block)
        if i < 6:
            block = [block[j] for j in MSG_PERMUTATION]
    return [
        state[i] ^ state[i + 8] for i in range(8)
    ] + [
        state[i + 8] ^ cv[i] for i in range(8)
    ]


def _words_from_bytes(block: bytes):
    block = block + b"\x00" * (BLOCK_LEN - len(block))
    return [
        int.from_bytes(block[i * 4:(i + 1) * 4], "little") for i in range(16)
    ]


def _words_to_bytes(words):
    return b"".join(int(w).to_bytes(4, "little") for w in words)


class _Output:
    __slots__ = ("cv", "block_words", "counter", "block_len", "flags")

    def __init__(self, cv, block_words, counter, block_len, flags):
        self.cv = list(cv)
        self.block_words = list(block_words)
        self.counter = counter
        self.block_len = block_len
        self.flags = flags

    def chaining_value(self):
        return _compress(
            self.cv, self.block_words, self.counter, self.block_len, self.flags
        )[:8]

    def root_bytes(self, length=OUT_LEN):
        out = bytearray()
        counter = 0
        while len(out) < length:
            words = _compress(
                self.cv,
                self.block_words,
                counter,
                self.block_len,
                self.flags | ROOT,
            )
            out.extend(_words_to_bytes(words))
            counter += 1
        return bytes(out[:length])


def _chunk_output(chunk: bytes, chunk_counter: int):
    cv = list(IV)
    if not chunk:
        return _Output(cv, [0] * 16, chunk_counter, 0, CHUNK_START | CHUNK_END)
    blocks = [chunk[i:i + BLOCK_LEN] for i in range(0, len(chunk), BLOCK_LEN)]
    for index, block in enumerate(blocks):
        flags = 0
        if index == 0:
            flags |= CHUNK_START
        if index == len(blocks) - 1:
            flags |= CHUNK_END
        words = _words_from_bytes(block)
        if index == len(blocks) - 1:
            return _Output(cv, words, chunk_counter, len(block), flags)
        cv = _compress(cv, words, chunk_counter, BLOCK_LEN, flags)[:8]
    raise AssertionError("unreachable")


def _parent_output(left_cv, right_cv):
    return _Output(list(IV), list(left_cv) + list(right_cv), 0, BLOCK_LEN, PARENT)


def _subtree_output(chunks, counter_start):
    if len(chunks) == 1:
        return _chunk_output(chunks[0], counter_start)
    left_len = 1
    while left_len * 2 < len(chunks):
        left_len *= 2
    left = _subtree_output(chunks[:left_len], counter_start)
    right = _subtree_output(chunks[left_len:], counter_start + left_len)
    return _parent_output(left.chaining_value(), right.chaining_value())


def blake3_256(data) -> bytes:
    """Unkeyed BLAKE3 with a 32-byte output."""
    if not isinstance(data, (bytes, bytearray, memoryview)):
        raise TypeError("blake3_256 expects bytes")
    data = bytes(data)
    chunks = [data[i:i + CHUNK_LEN] for i in range(0, len(data), CHUNK_LEN)]
    if not chunks:
        chunks = [b""]
    return _subtree_output(chunks, 0).root_bytes(OUT_LEN)


# Known-answer vectors. The first three are the published BLAKE3 vectors; the
# fourth is a multi-chunk vector whose expected digest was produced by the
# official `blake3` Python package inside the WeftFS python court and recorded
# here as a constant. This module does not compute its own expectations.
KAT = (
    (b"", "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"),
    (b"abc", "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"),
    (b"hello world", "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24"),
    (bytes(range(256)) * 11 + b"weft-context-plane",
     "a3968717901a97ef4193b222aa7a3b9e5d2c720e351c0075f8b9c5035f440c05"),
)


def self_test() -> None:
    for message, expected in KAT:
        got = blake3_256(message).hex()
        if got != expected:
            raise AssertionError(
                "BLAKE3 KAT failed for %d bytes: %s != %s"
                % (len(message), got, expected)
            )


if __name__ == "__main__":
    self_test()
    print("blake3: self-test OK")
