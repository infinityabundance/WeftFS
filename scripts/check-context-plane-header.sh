#!/usr/bin/env bash
# Compile-only gate for `include/weft_context_plane.h`.
#
# The header is a declaration-only candidate ABI (spec/rfc/RFC-0001-api.md).
# This gate proves it is valid C11 and C++17 under -Wall -Wextra -Werror,
# without compiling a library, exporting a symbol or linking anything.
#
# Host-runnable and court-runnable: `cc`/`c++` exist on a plain GitHub runner
# and `gcc`/`g++` exist in the rust court via build-essential. Override with
# CC / CXX when a court names the compiler differently.
#
# Exit 0 on success; exit 78 with exactly one `context_plane_header: skip ...`
# line when no suitable compiler is available; exit 1 on any diagnostic.
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

CC_BIN="${CC:-cc}"
CXX_BIN="${CXX:-c++}"

skip() {
  echo "context_plane_header: skip $*"
  exit 78
}

if ! command -v "$CC_BIN" >/dev/null 2>&1; then
  skip "no C compiler '$CC_BIN'"
fi
if ! command -v "$CXX_BIN" >/dev/null 2>&1; then
  skip "no C++ compiler '$CXX_BIN'"
fi

cflags=(-std=c11 -Wall -Wextra -Werror -pedantic -fsyntax-only -Iinclude)
cxxflags=(-std=c++17 -Wall -Wextra -Werror -pedantic -fsyntax-only -Iinclude)

echo "context_plane_header: $("$CC_BIN" --version | head -1)"
echo "context_plane_header: $("$CXX_BIN" --version | head -1)"

echo "--- C11: tests/c/test_context_plane_header.c"
"$CC_BIN" "${cflags[@]}" tests/c/test_context_plane_header.c

echo "--- C++17: tests/c/test_context_plane_header.cpp"
"$CXX_BIN" "${cxxflags[@]}" tests/c/test_context_plane_header.cpp

# The header must also be usable on its own, with no test translator unit.
echo "--- C11: header only"
"$CC_BIN" "${cflags[@]}" -x c include/weft_context_plane.h
echo "--- C++17: header only"
"$CXX_BIN" "${cxxflags[@]}" -x c++ include/weft_context_plane.h

echo "context_plane_header: OK (declaration only; no symbol exported, no library linked)"
