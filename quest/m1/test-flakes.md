# [M] Tests hold up under load

## Goal

Three tests that pass alone but fail under a full `just check` pass reliably,
fixed at the cause rather than by raising a timeout or adding a retry:

- `js/json/src/snapshot/snapshot.test.ts:359`, "a compressed delta is gated
  on its encoded size", which takes about 4.3 s against a 5 s limit.
- The `js/net/src/declarations.test.ts` test that times out at 5 s.
- `rs/moq-tokio/tests/backend.rs:739` `noq_cert_reload`, which fails with
  "Too many open files".

## Plan

- Find why each JS test is slow. It should shrink its input or reveal a real
  slowdown in the code under test; fix whichever it is.
- For `noq_cert_reload`, find what holds the descriptors: a leak in the test
  or code under test, or nextest parallelism against the file limit. Fix a leak
  at its source, and otherwise cap the test's concurrency in
  `.config/nextest.toml`.
- Prove it by running `just check --all` several times on a loaded machine.
