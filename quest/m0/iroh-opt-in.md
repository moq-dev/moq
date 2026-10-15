# [XS] iroh off by default in moq-cli

## Goal

`moq-cli` no longer builds iroh by default, matching `moq-relay`, `moq-bench`,
and `moq-tokio`: `cargo build`, `cargo install moq-cli`, and every
distribution (release tarballs, deb, rpm, Homebrew, winget, Docker, the Nix
package) ship without it, and `--features iroh` brings it back for a source
build. Iroh support itself stays.

## Plan

Decided with the maintainer on 2026-10-13:

- Why: iroh's dependency tree slows every source build and `cargo install`
  for an experimental transport few use.
- Releases drop it too. Nobody is believed to use iroh from a released
  binary, so there is no release-only feature and no per-pipeline flag.
  Released `moq` loses its `--iroh-*` flags; clap refuses them as unknown
  arguments, which is loud enough.
- This is an exception to the default-features rule (on unless the build
  host needs a system library or tool): an experimental transport with a
  heavy dependency tree stays off. Only nightly `just rs features`
  (`--all-features`) compiles moq-cli's iroh glue afterwards; that is
  accepted for a handful of `#[cfg(feature = "iroh")]` lines.
- `moq-relay` already has iroh off in source and release builds and stays
  that way.
- P2P does not use iroh; see [P2P](/quest/m1/p2p/README.md).

Work:

- `rs/moq-cli/Cargo.toml`: remove `iroh` from `default`.
- `demo/pub/justfile`: the `iroh` recipe passes `--iroh-enabled`, so it
  builds and runs with `--features iroh`.
- Docs: `doc/concept/transport.md`'s iroh section says a `moq` build needs
  `--features iroh`, the way it already says so for the relay, and the
  `doc/bin/cli.md` play install line drops `iroh` from its feature list.
- Verify the default build, `--features iroh`, and `--no-default-features`
  of `moq-cli` all compile and pass tests.

## Related

- [Bump web-transport-iroh for the capsule close](/quest/m1/iroh-capsule-bump.md) - still applies to `--features iroh` builds
