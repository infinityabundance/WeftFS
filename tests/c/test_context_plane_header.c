/*
 * Compile-only test for `include/weft_context_plane.h`.
 *
 * This translation unit is never linked and never run. It exists so that
 * `cc -std=c11 -Wall -Wextra -Werror -fsyntax-only` proves the declaration is
 * valid C11, that the ABI major is 1, that the negotiation fields sit at
 * frozen offsets, and that the function-table types can be assigned without
 * casts or warnings.
 */

#include <stddef.h>
#include <stdint.h>

#include "weft_context_plane.h"

_Static_assert(WEFT_CONTEXT_ABI_MAJOR == 1u, "ABI major must be 1");
_Static_assert(WEFT_CONTEXT_OK == 0, "WEFT_CONTEXT_OK must be zero");
_Static_assert((int)WEFT_CONTEXT_STATUS_MAX == 7, "documented status count");
_Static_assert(offsetof(weft_context_api_v1, struct_size) == 0,
               "struct_size must be the first field");
_Static_assert(offsetof(weft_context_api_v1, abi_major) == 4,
               "abi_major offset is frozen");
_Static_assert(offsetof(weft_context_api_v1, abi_minor) == 8,
               "abi_minor offset is frozen");
_Static_assert(offsetof(weft_context_api_v1, open) == 16,
               "table function offsets are frozen in ABI major 1");

static weft_context_status probe_table(weft_context_api_v1 *table)
{
    weft_context_session *session = NULL;
    weft_context_buffer *buffer = NULL;
    weft_context_slice slice;
    weft_context_status status;

    if (table == NULL || table->struct_size != sizeof(*table)) {
        return WEFT_CONTEXT_INVALID_ARGUMENT;
    }
    if (table->abi_major != WEFT_CONTEXT_ABI_MAJOR) {
        return WEFT_CONTEXT_ABI_MISMATCH;
    }

    status = table->open("weftfs://local", (size_t)13, table, &session);
    if (status != WEFT_CONTEXT_OK) {
        return status;
    }
    status = table->invoke(session, (const uint8_t *)"", (size_t)0, &buffer);
    if (status == WEFT_CONTEXT_OK && buffer != NULL) {
        status = table->buffer_view(buffer, &slice);
        if (status == WEFT_CONTEXT_OK && slice.data == NULL && slice.len != 0) {
            status = WEFT_CONTEXT_BUFFER_INVALID;
        }
        (void)table->buffer_release(buffer);
    }
    (void)table->close(session);
    return status;
}

int main(void)
{
    weft_context_api_v1 table;

    table.struct_size = (uint32_t)sizeof(table);
    table.abi_major = WEFT_CONTEXT_ABI_MAJOR;
    table.abi_minor = WEFT_CONTEXT_ABI_MINOR;
    table.reserved0 = 0;
    table.open = NULL;
    table.close = NULL;
    table.invoke = NULL;
    table.buffer_view = NULL;
    table.buffer_release = NULL;

    /* The discovery declaration must be addressable without a definition. */
    weft_context_status (*discover)(weft_context_api_v1 *) =
        weft_context_get_api_v1;
    (void)discover;
    (void)probe_table;
    return 0;
}
