# [S] Tenant matrix on both runtimes

## Goal

A committed moq-relay integration test runs one cross-tenant matrix against
a real relay on the shared Tokio QUIC listener and again on io_uring
workers, so a change to either runtime's admission path that lets one tenant
reach another fails CI.

## Plan

From the 2026-10-28 external security audit, whose follow-up ran this
matrix as a scratch harness at
[7ea27dd4d9](https://github.com/moq-dev/moq/commit/7ea27dd4d9) and passed on
both runtimes. Today `rs/moq-relay/tests/integration/` checks tenant roots,
mounts, and revalidation on Tokio only (`public_rules`, `auth_lifetime`,
`session_revalidate`); io_uring has `runtime_uring`'s refusal counter.
`refusals` checks both runtimes' SETUP Path refusal and request URI handling.

The matrix, with credential `a` granted root `tenant-a` and `b` granted
`tenant-b` whatever URL they dial:

1. A publishes `public/cam` and `secret/cam`, B publishes `probe`; a
   subscriber with A's grant sees A's broadcasts and not B's.
2. B never sees A's `secret`.
3. B's read-only mount `shared -> tenant-a/public` announces and delivers
   `shared/cam`.
4. B publishes `shared/pwn`, then an allowed marker on the same session;
   once B sees the marker, A has still not seen `pwn`. The audit's run kept
   B's session open after the refusal; if a refusal ever closes it, the close
   is the ordering point instead.
5. `GET /announced/tenant-a?jwt=b` lists B's root, not A's secret, and
   `GET /fetch/tenant-a/secret/cam/video?jwt=b` is not 200.
6. A connect request whose raw target is `/tenant-a/../tenant-b?jwt=…` is
   admitted as `/tenant-b` with its query intact, or refused, never as
   `tenant-a`.
7. Moving a live session's grant root from `tenant-b` to `tenant-a` closes it
   as unauthorized.

Run checks 1 to 4, 6, and 7 in one body for both runtimes, gating the
io_uring run like `runtime_uring.rs` (Linux, the `_uring` feature, loud skip
on unsupported kernels unless `CI_STRICT`). Run check 5 once: the relay's
HTTP listener is the Tokio web stack in both modes, and the io_uring fixture
configures only QUIC.

Check 6 must put the dot segments on the wire. The audit's harness and the
suite's WebSocket helper build the target through `url::Url`, which removes
`..` before sending, so the audit's pass for this check only showed the
client normalizing. Send a hand-built request target instead. Reuse the suite's existing relay and auth
helpers. The audit's scratch harness was in the main checkout's
`.tmp/auth-runtime-audit/src/main.rs` (274 lines); it may be gone, and the
list above is the spec.

Decided 2026-10-28 (maintainer): adopt the matrix; Miri and sanitizer
coverage were not planned, since they can't run the syscall and C-caller
paths and the audit's unsafe review found nothing.

Public API: none. Wire: none.
