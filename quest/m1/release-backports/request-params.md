# [S] Unknown request parameters on release

## Goal

On `release`, a draft 14-17 peer that sends FORWARD on SUBSCRIBE_NAMESPACE, or
any unlisted request parameter on drafts 14 and 15, keeps its session instead
of closing it with InvalidValue.

## Plan

#5028 (`4bf8b59cd`) fixed this on `main` by reworking `decode_params!`, which
depends on main-only macro syntax. Release's `request_stream.rs` (from #5133)
differs from main's, so adapt that part rather than cherry-picking it. Write
the minimal release patch: accept FORWARD where drafts 15-17 allow it, and skip unknown
keys where drafts 14 and 15 say to ignore them. moqx on draft 16 is the known
peer that hits this; it redials in a loop today.
