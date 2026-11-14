/*
 * weft_context_plane.h — candidate in-process C ABI for the WeftFS open
 * context plane (draft RFC 0001, companion document `spec/rfc/RFC-0001-api.md`).
 *
 * STATUS, EXACTLY AS THE COMPANION DOCUMENT STATES IT
 * ---------------------------------------------------
 * This header is a compilable INTERFACE DECLARATION ONLY. It contains no
 * implementation, no exported symbol, no shared library, no existing
 * integration and no network endpoint. It is NOT part of the current WeftFS
 * published library API, and compiling or linking against it does not create
 * one. It is NOT a binary-compatibility certification: no implementation is
 * shipped here, so no ABI compatibility has been demonstrated.
 *
 * Ownership and failure rules (§9 and §12 of the companion document):
 *   - All buffers returned through `invoke` are OWNED BY THE PRODUCING
 *     LIBRARY. A caller must not free, reallocate or retain them beyond the
 *     lifetime of the producing library.
 *   - Input byte slices are borrowed only for the duration of the call.
 *   - `buffer_view` returns an immutable borrowed view valid only while the
 *     owned output buffer exists.
 *   - No function may unwind a Rust panic or propagate a C++ exception
 *     through the boundary. Language-runtime failures are converted into a
 *     nonzero `weft_context_status` inside the library.
 *   - Each session is SINGLE-THREADED unless a separately negotiated
 *     extension states otherwise; the base v1 ABI defines no concurrency
 *     guarantee.
 *   - Callers must keep the producing library loaded until every session and
 *     every buffer has been released.
 *
 * Version negotiation: a caller zeroes `weft_context_api_v1`, sets
 * `struct_size` to `sizeof(weft_context_api_v1)`, sets `abi_major` to 1 and
 * `abi_minor` to the minimum minor revision it can use, then calls
 * `weft_context_get_api_v1`. The function fills the table and reports the
 * negotiated minor revision. A major mismatch, a minor revision the library
 * cannot provide, or an undersized table is an ABI failure and no table is
 * returned. Future minor revisions may add trailing fields only; offsets of
 * every existing field are frozen and a newer major revision requires a new
 * symbol and a new structure, never an in-place reinterpretation.
 *
 * The ABI deliberately handles context bytes and metadata only. It does not
 * transfer tensor memory, GPU device pointers, placement, scheduling or
 * transport; DLPack and Arrow remain the appropriate interop interfaces for
 * data exchange.
 */

#ifndef WEFT_CONTEXT_PLANE_H
#define WEFT_CONTEXT_PLANE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ABI major/minor of this declaration. */
#define WEFT_CONTEXT_ABI_MAJOR 1u
#define WEFT_CONTEXT_ABI_MINOR 0u

/*
 * An implementation may define WEFT_CONTEXT_API to its export/visibility
 * macro before including this header. The declaration-only build defines
 * nothing and exports nothing.
 */
#ifndef WEFT_CONTEXT_API
#define WEFT_CONTEXT_API
#endif

/*
 * Status codes. `WEFT_CONTEXT_OK` means an envelope was delivered, even when
 * that envelope itself contains `outcome:"error"`: a semantic `denied` or
 * `unproven` verdict is a successful API invocation. Any nonzero status means
 * no trustworthy response was delivered.
 *
 * The numeric values below are part of the candidate ABI major 1 contract.
 * `WEFT_CONTEXT_STATUS_MAX` is a count of the documented statuses, not a
 * status an implementation may return.
 */
typedef int32_t weft_context_status;

enum {
    WEFT_CONTEXT_OK = 0,
    WEFT_CONTEXT_INVALID_ARGUMENT = 1,
    WEFT_CONTEXT_ABI_MISMATCH = 2,
    WEFT_CONTEXT_OPEN_FAILED = 3,
    WEFT_CONTEXT_SESSION_CLOSED = 4,
    WEFT_CONTEXT_BUFFER_INVALID = 5,
    WEFT_CONTEXT_INTERNAL = 6,
    WEFT_CONTEXT_STATUS_MAX = 7
};

/* Opaque handles. Their layout is private to the producing library. */
typedef struct weft_context_session weft_context_session;
typedef struct weft_context_buffer weft_context_buffer;

/* A borrowed, immutable byte view. Never owned by the consumer. */
typedef struct weft_context_slice {
    const uint8_t *data;
    size_t len;
} weft_context_slice;

/* Forward declaration: `open` receives the negotiated table so an
 * implementation can bind a session to the exact minor revision the caller
 * agreed to. */
typedef struct weft_context_api_v1 weft_context_api_v1;

/*
 * `open` binds a session to a trusted local configuration. `config_uri` is a
 * UTF-8, non-NUL-terminated byte slice supplied by the caller and borrowed
 * only for the call. On success the library writes an opaque session handle
 * to `*out_session`; on failure it writes NULL and returns a nonzero status.
 */
typedef weft_context_status (*weft_context_open_fn)(
    const char *config_uri,
    size_t config_uri_len,
    const weft_context_api_v1 *api,
    weft_context_session **out_session);

/*
 * `close` releases a session. It returns WEFT_CONTEXT_INVALID_ARGUMENT for a
 * NULL handle and WEFT_CONTEXT_SESSION_CLOSED for an already-closed handle.
 * Buffers previously returned by `invoke` remain valid until individually
 * released with `buffer_release`, but only while the producing library is
 * still loaded.
 */
typedef weft_context_status (*weft_context_close_fn)(
    weft_context_session *session);

/*
 * `invoke` accepts exactly one canonical WCB `weft.context.request.v1`
 * envelope. `request` is borrowed for the duration of the call. On success it
 * writes a library-owned opaque buffer to `*out_buffer`; the buffer holds one
 * canonical WCB `weft.context.response.v1` envelope. Structural or
 * authorization failures are carried inside that envelope; delivery failures
 * are returned as a nonzero status and leave `*out_buffer` NULL.
 */
typedef weft_context_status (*weft_context_invoke_fn)(
    weft_context_session *session,
    const uint8_t *request,
    size_t request_len,
    weft_context_buffer **out_buffer);

/*
 * `buffer_view` returns an immutable borrowed view over a library-owned
 * output buffer. The view is valid only while the buffer exists. The caller
 * must not free or modify `out_slice->data`.
 */
typedef weft_context_status (*weft_context_buffer_view_fn)(
    const weft_context_buffer *buffer,
    weft_context_slice *out_slice);

/*
 * `buffer_release` releases one library-owned output buffer exactly once. The
 * call must come from the same producing library that created it.
 */
typedef weft_context_status (*weft_context_buffer_release_fn)(
    weft_context_buffer *buffer);

/*
 * The versioned function table. Field order and offsets are frozen within ABI
 * major 1. New optional functions may be appended in a later minor revision;
 * no existing field may move or change type.
 *
 * `struct_size` is the size of the table the caller compiled against.
 * `abi_major` must be WEFT_CONTEXT_ABI_MAJOR. `abi_minor` is negotiated: the
 * library returns the highest minor revision it supports that is not greater
 * than the caller's requested minimum. `reserved0` must be zero.
 */
struct weft_context_api_v1 {
    uint32_t struct_size;
    uint32_t abi_major;
    uint32_t abi_minor;
    uint32_t reserved0;
    weft_context_open_fn open;
    weft_context_close_fn close;
    weft_context_invoke_fn invoke;
    weft_context_buffer_view_fn buffer_view;
    weft_context_buffer_release_fn buffer_release;
};

/*
 * The only required discovery symbol of ABI major 1. This is a DECLARATION:
 * this header neither defines nor exports it. A caller that wants a
 * fail-closed check should load the symbol dynamically and treat absence as
 * "no context-plane implementation available", never as success.
 *
 * On entry `table` must point to writable storage of at least `struct_size`
 * bytes. The function returns WEFT_CONTEXT_OK and fills the table on success;
 * it returns WEFT_CONTEXT_ABI_MISMATCH (or WEFT_CONTEXT_INVALID_ARGUMENT for a
 * NULL table) and leaves no usable table on failure.
 */
WEFT_CONTEXT_API weft_context_status weft_context_get_api_v1(
    weft_context_api_v1 *table);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* WEFT_CONTEXT_PLANE_H */
