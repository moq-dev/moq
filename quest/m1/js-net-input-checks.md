# [M] js/net and moq-net decode the same inputs the same way

## Goal

Rust and JS cannot drift apart on what they accept, because both run one shared corpus, and URLs are parsed once into a typed endpoint:

- A shared corpus of moq-lite inputs, each with its expected outcome (decoded value, or refused), is replayed by `rs/moq-net` and `js/net` tests. It starts from the fuzz regressions in `rs/moq-net/fuzz/regressions` plus hand cases for every difference this audit found: the lite-01/02 ANNOUNCE_INIT size and count budget (JS allows 64 MiB and checks the announce budget only after building the array), group sequences at 2^64 (`checkSequence` accepts them), and a pinned server version the client did not offer (`accept.ts`).
- JS parses a dial target once into a typed endpoint (scheme, host as an IP or a name, port), the way `rs/moq-net/src/reconnect/redirect.rs` does, and GOAWAY classification, redirect checks, dialing and log redaction all use it. Today `isLocal` (`js/net/src/connection/goaway.ts`) reads the opaque `moql:` host, so `127.1`, `0x7f000001` and `2130706433` pass as non-local while `dialable()` turns them into 127.0.0.1, which lets a relay steer a `redirect: "follow"` client and its tokens to loopback. `redact` logs `null/<path>` for `moql:` URLs, path tokens included.
- `open.ts` `patience` treats an RTT of 0 as unknown.

## Plan

Found by the 2026-11-07 invariant audit (js-2, js-5 to js-9).

Decided 2026-11-07 (maintainer, invariant-audit root causes): the cause is two hand-written decoders with no shared inputs; the corpus makes any divergence a failing test on both sides. Pick the corpus location and format to suit both test runners (a directory of JSON or hex cases is the obvious start). The single-implementation route stays [Evaluate WASM-to-JS for @moq/net](/quest/m5/rs2ts-wasm2js.md). js-1 (announce queue growth) is [JS announced coalescing](/quest/m3/js-announced-coalesce.md).
