/*
 * Compile-only C++ test for `include/weft_context_plane.h`.
 *
 * Proves the header is valid C++17 with `extern "C"` linkage, that the
 * versioned table can be default-initialised, and that the discovery symbol
 * has C linkage (taking its address is enough; nothing is linked).
 */

#include <cstddef>
#include <cstdint>
#include <type_traits>

#include "weft_context_plane.h"

static_assert(WEFT_CONTEXT_ABI_MAJOR == 1u, "ABI major must be 1");
static_assert(std::is_standard_layout<weft_context_api_v1>::value,
              "the ABI table must be standard-layout");
static_assert(offsetof(weft_context_api_v1, struct_size) == 0,
              "struct_size must be the first field");
static_assert(std::is_same<decltype(weft_context_get_api_v1(
                               static_cast<weft_context_api_v1 *>(nullptr))),
                           weft_context_status>::value,
              "discovery returns weft_context_status");
static_assert(std::is_same<weft_context_status, std::int32_t>::value,
              "status width is fixed at 32 bits");

int main()
{
    weft_context_api_v1 table{};
    table.struct_size = static_cast<std::uint32_t>(sizeof(table));
    table.abi_major = WEFT_CONTEXT_ABI_MAJOR;
    table.abi_minor = WEFT_CONTEXT_ABI_MINOR;

    auto *discover = &weft_context_get_api_v1;
    (void)discover;
    (void)table;
    return 0;
}
