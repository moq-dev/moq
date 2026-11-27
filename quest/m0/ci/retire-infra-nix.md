# [S] Hand off infra/nix and infra/mbx to shared nix.kixel.quest and public B2 mbx cache

## Goal

`moq-dev/moq` stops maintaining its own Nix binary cache Worker (`infra/nix/`) and per-repo `mbx` bucket config (`infra/mbx/`), moving onto the shared cross-repo `nix.kixel.quest` binary cache (which keeps `nix.moq.dev` aliased for external users) and the shared public Backblaze B2 `mbx` Rust cache:

- `flake.nix`, `.github/workflows/cache.yml`, and `.github/workflows/release-nix.yml` push through `quest cache push` (replacing `sh/gh/cache.sh`, `sh/gh/cache-runtime.sh`, and `infra/nix/justfile`), and `infra/nix/` is deleted once `nix.kixel.quest` serves `nix.moq.dev` with all existing release paths verified.
- Hosted cross-platform and Nightly `mbx` jobs switch from `mbx-moq-dev` to the shared public B2 `mbx` bucket so `moq` shares compiled crate artifacts across repositories, and `infra/mbx/` is retired.
- External `nix run github:moq-dev/moq/<tag>#moq-relay --accept-flake-config` continues substituting cleanly from `nix.moq.dev` and `nix.kixel.quest`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan`):

- ✅ Consolidate the Backblaze B2 Nix binary cache (already moved to B2 in [moq-dev/moq#9625](https://github.com/moq-dev/moq/pull/9625)) and public B2 `mbx` Rust cache (moved to B2 in [moq-dev/moq#9624](https://github.com/moq-dev/moq/pull/9624)) into shared cross-repo infrastructure hosted by `kixel.quest` rather than maintaining per-repo Workers and cache config inside `moq`.
- ✅ Scope this quest to the shared cache handoff and `infra/nix` / `infra/mbx` retirement only; leave migrating `moq`'s workflows and `sh/dispatch.sh` to `quest.toml` until smaller pilot repos prove the contract first (rejected: planning full `moq` `quest.toml` migration right away).

Implementation notes:

- Carry forward the ranged `If-Match` NAR resume fix (`moq-dev/moq#9604`, `#9625`) when `kixel.quest` takes over `nix.moq.dev`, and verify every existing release tag closure substitutes from `nix.moq.dev` and `nix.kixel.quest` in a clean container before deleting `infra/nix/`.
- Keep `nix.moq.dev` aliased to the shared cache Worker so existing pinned release flakes and user docs continue working with zero breakage.

Tests: clean container test substituting both the current `devShells.default` and a released `moq-relay` tag from `nix.moq.dev` and `nix.kixel.quest`; confirm a Nightly macOS/Windows run hits the shared public `mbx` B2 cache; `just check` passes after deleting `infra/nix/` and `infra/mbx/`.

## Waiting

- kixel-ai/kixel.quest: `nix.kixel.quest` (with `nix.moq.dev` alias and existing release paths copied) and the shared public B2 `mbx` bucket are live; check once `https://nix.kixel.quest/nix-cache-info` responds 200 and release tags substitute there.
- kixel-ai/quest: `quest cache push` ships on `main`; check once `quest cache push --help` is available in `moq`'s pinned `quest` flake input.

## Related

- [Confirm nix.moq.dev no longer drops NAR streams](/quest/m0/ci/nix-nar-stream.md) - verifies the B2 Worker's NAR resume behavior carried into `nix.kixel.quest`
- [R2's Rust cache is torn down](/quest/m0/ci/mbx-b2.md) - finishes tearing down the older R2 `mbx` bucket
- [Verify the B2 Rust cache](/quest/m0/ci/mbx-b2-verify.md) - measures B2 `mbx` hit rates and egress
