"""Ed25519 verification (RFC 8032) in pure Python.

Used only by the optional capability-envelope check. This is an independent
implementation from the RFC text, with extended-coordinate arithmetic and
canonical point decoding. It is not constant-time and makes no side-channel
claim; the verifier is an integrity tool, not a signing service.
"""

from __future__ import annotations

import hashlib

P = 2 ** 255 - 19
L = 2 ** 252 + 27742317777372353535851937790883648493
D = (-121665 * pow(121666, P - 2, P)) % P
I = pow(2, (P - 1) // 4, P)

IDENTITY = (0, 1, 1, 0)


def _inv(x: int) -> int:
    return pow(x, P - 2, P)


def _xrecover(y: int) -> int:
    xx = (y * y - 1) * _inv(D * y * y + 1) % P
    x = pow(xx, (P + 3) // 8, P)
    if (x * x - xx) % P != 0:
        x = x * I % P
    if x % 2 != 0:
        x = P - x
    return x


_BY = 4 * _inv(5) % P
_BX = _xrecover(_BY)
BASE = (_BX, _BY, 1, _BX * _BY % P)


def _add(a, b):
    x1, y1, z1, t1 = a
    x2, y2, z2, t2 = b
    aa = (y1 - x1) * (y2 - x2) % P
    bb = (y1 + x1) * (y2 + x2) % P
    cc = t1 * 2 * D * t2 % P
    dd = z1 * 2 * z2 % P
    e = bb - aa
    f = dd - cc
    g = dd + cc
    h = bb + aa
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def _double(a):
    x1, y1, z1, _ = a
    aa = x1 * x1 % P
    bb = y1 * y1 % P
    cc = 2 * z1 * z1 % P
    h = aa + bb
    e = h - (x1 + y1) * (x1 + y1) % P
    g = aa - bb
    f = cc + g
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def _multiply(point, scalar: int):
    result = IDENTITY
    addend = point
    while scalar:
        if scalar & 1:
            result = _add(result, addend)
        addend = _double(addend)
        scalar >>= 1
    return result


def _encode(point) -> bytes:
    x, y, z, _ = point
    zi = _inv(z)
    x = x * zi % P
    y = y * zi % P
    bits = [(y >> i) & 1 for i in range(255)] + [x & 1]
    return bytes(
        sum(bits[8 * byte + offset] << offset for offset in range(8))
        for byte in range(32)
    )


def _decode(raw: bytes):
    if len(raw) != 32:
        raise ValueError("point is not 32 bytes")
    value = int.from_bytes(raw, "little")
    sign = value >> 255
    y = value & ((1 << 255) - 1)
    if y >= P:
        raise ValueError("non-canonical y")
    x = _xrecover(y)
    if x & 1 != sign:
        x = P - x
    point = (x, y, 1, x * y % P)
    if (y * y - x * x - 1 - D * x * x * y * y) % P != 0:
        raise ValueError("point is not on the curve")
    # Reject the identity and the other small-order points. The identity is
    # the only one with x == 0 and y == 1; an order-2 point has y == -1.
    if x == 0 and y == 1:
        raise ValueError("identity public key")
    if y == P - 1:
        raise ValueError("small-order public key")
    return point


def verify(public_key: bytes, message: bytes, signature: bytes) -> bool:
    if len(public_key) != 32 or len(signature) != 64:
        return False
    try:
        a = _decode(public_key)
        r = _decode(signature[:32])
    except ValueError:
        return False
    s = int.from_bytes(signature[32:], "little")
    if s >= L:
        return False
    k = int.from_bytes(
        hashlib.sha512(signature[:32] + public_key + message).digest(), "little"
    ) % L
    left = _multiply(BASE, s)
    right = _add(r, _multiply(a, k))
    return _encode(left) == _encode(right)
