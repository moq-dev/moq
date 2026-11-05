# [M] Cargo feature review before the cut

## Goal

Every Cargo feature in the workspace has been judged before the release
ships. Defaults build nothing expensive or system-dependent. Each feature that
remains lives in the crate that owns its integration (`s3` in moq-archive,
forwarded by the binaries that ship it). A feature that only trims compile
time is removed in favor of always-compiled code. The scope covers the
defaults and `release` sets of shipped binaries, and the feature lists the
bindings and packaging builds pass. Each change the maintainer approves is a
quest that lands before the cut.

## Plan

Recurs once per major release (decided 2026-10-26). The [release
cut](/quest/m2/release/cut.md) re-creates this quest for the next release when
it shifts the milestones, so the PR finishing this run adds no successor.
Record what this run learned (inventory method, findings, what to check
first) in the cut's PR notes or a follow-up quest, so the next run starts from
it. Only `/quest-delete` ends the recurrence.

Starts once m1 lands, so the features it reviews are the ones that ship; the
cut waits for it. Feature changes break downstream builds, which is why this
runs before the cut rather than with the post-release audits. Like the [API
shape review](/quest/m2/release/api-review.md), this run changes no code
(decided 2026-10-26): it proposes each change with a recommendation, asks the
maintainer, and files each approved change as a quest in m1, which holds the
cut until it lands.

Judge every feature against the `# Features` rules in `rs/AGENTS.md`, and
challenge each one's premise: fewer features are better. A feature trims a
build for a few power users but fragments the code and its test matrix, so
keep one only for a vendor integration or a system dependency. For each crate,
inventory the `[features]` table, which crates forward each feature, and what
it gates. Then check:

- Defaults: nothing that needs libclang, a system library at build or load
  time, or a large dependency tree. A shipped binary's `release` set holds
  every backend its platform loads without a system library.
- Scope: a vendor feature sits in the crate that integrates the vendor, and
  dependents forward it under the same name instead of enabling their
  dependencies' features directly.
- Fragmentation: features no consumer turns off, or that gate logic rather
  than a dependency, are candidates to delete.

What to check first (seeded 2026-10-26):

- moq-cli's `s3`, `gcs`, and `azure` enable `object_store/aws`, `gcp`, and
  `azure` directly; moq-archive, which owns storage, has no features. Also
  check moq-cli's other defaults (`cluster-lan`, `quic`, `websocket`, `nvidia`)
  against the rules. Distributions building with `release` is
  [Ship capture and playback](/quest/m2/cli-packaging.md)'s work, not this
  review's.
- The crates with the most features at seeding: moq-cli (21), moq-tokio (17),
  moq-relay (14), moq-native (13), and moq-video (11).

Related: the [compile time audit](/quest/m2/audit-compile-time.md) owns
per-crate build cost, and the [dependency audit](/quest/m2/audit-deps.md) owns
which dependencies exist.

## Required

- [Breaking release readiness](/quest/m1/README.md) - the release's features are final before they are reviewed
