Repository-specific review rules for `moq` (extends Quest's shared review rules, read from the base branch).
[RISK.md](RISK.md) defines low, medium, and high risk tiers; judge a PR's tier by it.

# Checks

- **Layer**: does logic live where it belongs (library vs runtime adapter, app vs relay, moq-lite model vs IETF layer, our crate vs upstream)? Does a package boundary leak?
- **Public API & wire**: would we support this shape for years, in every language? Look for foot-guns, names tied to today's implementation, surfaces that could merge, and unneeded exports. Check that API/wire changes appear under Impact, with breaking ones marked `!`.
- **Return what callers use**: an outcome enum earns its place only when callers handle variants differently. Flag enums callers collapse identically, `bool`/`Option`/`Poll`/`Result` in disguise, and error variants representing normal outcomes.
- **Bounds**: can a peer grow a queue or map here? Hold latest state per key, or bound by credit the peer already spent, never one entry per event.

# State

Flag new or changed lifecycle code that breaks these (make bad combinations unrepresentable rather than checking them at runtime):

- **One enum per lifecycle**: phases are one enum/union whose variants carry their data (like `PresentState` in `rs/moq-net/src/lite/session.rs` and `State` in `js/net/src/ietf/join.ts`). Flag flags/`Option`/errors beside an enum, duplicated terminal state, `.expect()`/call-order-dependent data, and one-shot handles taken before validation.
- **Terminal after the work**: mark finished/closed/answered only after fallible work (or record failure as terminal state). Use consuming methods like `finish(self)`. Flag "done" set before a fallible flush/write.
- **One lock per value**: never guard one value with two locks or split across atomics (`CompletionState` in `rs/moq-gst/src/sink/session.rs`).

# Tests

Flag unit tests waiting on the real clock in any language, even via `kio::clock` or `@moq/clock` (they still wait in real time until a paused/simulated/`Manual` clock is installed). Fix with a mocked clock or awaited event in every language's sibling test. Only tests driving real sockets, devices, or processes may opt out via `host_time()` / `hostTime()`.

# Models

When a PR changes a file in a `verify-*` skill's Coverage table, run that skill on the head. Flag covered changes the model doesn't mirror, configs whose result changed without updating `\* Open finding:`, and stale sync commits not moved to the PR's base. The flow models under `js/*/verify/` have no skill: when a PR changes code their README names, apply the same check and run the affected configs as the README shows.

# Not findings

- Detecting a peer's duplicate/reused IDs or a publisher's reused sequences when only the sender's own requests/broadcast suffer.
- A compatibility shim, deprecation path, or migration for something no release has shipped (check `release` and tags; `main` and `-wip` wire versions break freely).
- A `moq-transport` MUST or "spec violation" with no behavioral consequence, or extra machinery for an IETF-only feature `moq-lite` lacks.
- Cross-node or cross-session waiting to close a rare race (reporting current state and accepting rare undercounts is intended).
- Cross-language parity without first confirming the other side implements the feature.
