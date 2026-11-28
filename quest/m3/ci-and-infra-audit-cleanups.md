# [S] Clean up temporary CI workflows, B2 worker 403 handling, rust2 pin, and infra docs

## Goal

Four small infra and CI loose ends surfaced by the 2026-11-28 audit are cleaned up:
1. Temporary `.github/workflows/central-check.yml` (which runs `kixel-ai/rust2:main` code with `SIGNING_KEY` and `MAVEN_CENTRAL_*`) is deleted once Maven Central validation is no longer needed.
2. `infra/nix/src/worker.ts` distinguishes B2 cap/signature `401`/`403` errors instead of disabling the cache for 60 s on any 403, and the first `0.22` tag's `Release Nix` B2 push is verified.
3. `sh/rs/instructions.sh` drops its hardcoded `"moq-pattern matching"` entry once `delete-moq-pattern` lands, and `flake.nix`'s frozen `rust2` pin (`cda8fc3e`) is reconciled with `kixel-ai/swole`.
4. `infra/README.md:45-48` reflects the narrowed `moq-sh` token scope instead of the old account-wide `Workers Editor` role.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ batch these four non-blocking infra/CI cleanups in `m3`.

Caused by [moq-dev/moq#9519](https://github.com/moq-dev/moq/pull/9519), [moq-dev/moq#9520](https://github.com/moq-dev/moq/pull/9520), [moq-dev/moq#9625](https://github.com/moq-dev/moq/pull/9625), [moq-dev/moq#9632](https://github.com/moq-dev/moq/pull/9632), [moq-dev/moq#9555](https://github.com/moq-dev/moq/pull/9555), [moq-dev/moq#9755](https://github.com/moq-dev/moq/pull/9755), and [moq-dev/moq#9517](https://github.com/moq-dev/moq/pull/9517).

- **Delete `central-check.yml` (`#9519`, `#9520`):** `.github/workflows/central-check.yml:28-75` and `.github/justfile:141-144` run `ffi/release/central.sh` from `kixel-ai/rust2:main` inside `moq-dev/moq`'s `publish` Environment with `SIGNING_KEY`, expanding the GPG signing key's trust boundary with no tracking quest to remove it. Delete once Maven Central validation is finished.
- **`nix.moq.dev` B2 Worker (`#9625`, `#9632`):** inspect B2 XML error codes in `infra/nix/src/worker.ts:179-182` before returning 500, and verify the first `0.22` release tag's `Release Nix` upload to B2 succeeds (`#9632` closed `nix-b2` without exercising a real tag push).
- **Instruction benchmarks (`#9555`, `#9755`):** remove `"moq-pattern matching"` from `sh/rs/instructions.sh:44` after delete-moq-pattern lands, and move off the frozen `rust2` pin `cda8fc3e` once the `swole` bench harness migration lands.
- **Docs (`#9517`):** update `infra/README.md:45-48` to match `quest/m3/release/moq-sh-deploy.md:20-22` (`Workers Scripts Write`, `Account Settings Read`, `Workers Routes Write`).

Public API: none. Wire: none.
