# [XS] Bump the Rust cache to the mbx manifest-size fix

## Goal

Condition: an mbx release contains
[jdx/mr-boxington#798](https://github.com/jdx/mr-boxington/pull/798), the fix
for [jdx/mr-boxington#792](https://github.com/jdx/mr-boxington/issues/792).
It merged after v1.27.0 was tagged; the open release PR
[jdx/mr-boxington#797](https://github.com/jdx/mr-boxington/pull/797) proposes
it as 1.27.1. A tag carries the fix when
`gh api repos/jdx/mr-boxington/compare/<tag>...9c8d3c7eee60e5fedb56fd4a0f07bd0caf8df249 --jq .status`
prints `behind` or `identical`.

Then set `version:` in `.github/actions/rust-cache/action.yml` to that
release, past the 1.25.0 hold from
[#6515](https://github.com/moq-dev/moq/pull/6515), and replace the hold's
comment. The release also carries
[jdx/mr-boxington#789](https://github.com/jdx/mr-boxington/pull/789) (in
1.27.0), the suspected cause of 1.26's empty `.rmeta` files. After it lands,
the merge queue shows no `error[E0463]: can't find crate` and the next
scheduled Nightly's macOS job logs no new `over the 16777216 byte limit`
digest.

## Plan

#798 caps a task manifest at a 15 MiB serialized budget below the reader's
16 MiB bound, but does not repair manifests already oversized on R2. Those
stay unreadable and unwritable, since the conditional PUT fails, until the
30-day rule in `infra/mbx/lifecycle.json` deletes them. If task identities
carry over to the new version, expect refusals for the three known macOS
digests until about 2026-11-22 (`a90076a4...`, `5cb8fb79...`, `b0310ce4...`);
a refusal for any other digest means the fix did not hold.

## Related

- [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) - measures hosted reuse once this lands
