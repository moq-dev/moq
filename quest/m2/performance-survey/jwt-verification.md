# [M] Attribute authentication verification cost before caching

## Goal

Authentication cost is explained across key sources and concurrent clients,
and avoidable work is reduced without weakening key rotation or validation.

## Plan

[Serve::verify](/rs/moq-auth/src/serve.rs) rereads a key or whole key-set
file for each request, then imports it (lines 249-271). A Key's cached crypto
material is therefore not shared across these requests. The
[KeySet lookup](/rs/moq-auth/src/set.rs) is linear, while header parsing also
occurs in [Key::decode](/rs/moq-auth/src/key.rs). Existing session admission
and relay lease-refresh benchmarks do not isolate actual JWT verification.

Benchmark clients × keys, each supported algorithm, first/last/missing kid,
file/directory/set sources and cold/warm material separately. Attribute file
I/O, JSON/JWT parsing, lookup, key import and signature verification; record
CPU, allocations, throughput and p95/p99 request latency. Include concurrent
key replacement, removal, invalid keys, expired tokens and refusal bursts.
Use deterministic time in validity checks.

Do not prescribe a TTL or token-result cache. Preserve current fresh-key and
revocation behavior; if sharing requires a new freshness contract, present
that decision with measurements before implementing it. This extracts and
sharpens the recurring audit's auth benchmark gap.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
