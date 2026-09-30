# [S] One-command moq installation and upgrades

## Goal

A canonical Bash installer installs the released `moq` binary on supported
macOS and Linux machines without Rust or sudo. Running it again upgrades
the same installation; selecting a version supports
reproducible installs and deliberate downgrades.

Install only `moq` from the `moq-cli` release. Token and relay functionality
belong to its subcommands, not separate installer choices. This work does
not implement those subcommands, automatic updates, a self-update command,
service setup, Windows support, or new release targets.

## Plan

Deferred to m2 in the 2026-09-30 audit: no named consumer; `cargo install`,
Nix, Docker, and winget already install `moq`.

- Default to the latest stable `moq-cli` release, with an explicit version
  option. Resolve that product's tags, not the repository-wide latest
  release: this repository publishes multiple independently versioned crates.
  Refuse missing versions, malformed input, and incomplete releases clearly.
- Verify the selected archive against the release's `SHA256SUMS`, extract it
  into a temporary file in the destination directory, then atomically rename
  it over the destination. A failed install leaves the prior binary untouched.
  Decided in the 2026-09-30 audit: no journaled transaction or ownership
  record; that was out of proportion for one binary.
- Support the existing targets: macOS ARM64 and Linux x86_64/ARM64 with
  glibc 2.34 or newer. Refuse unsupported operating systems, architectures,
  and libc variants with actionable diagnostics.
- Default to `~/.local/bin` with an explicit directory override. Do not invoke
  sudo or edit shell profiles. Print the installed version and path, and PATH
  instructions when needed. Warn when another `moq` on PATH takes precedence.
  Refuse a destination that is a symlink, so a package manager's install is
  never overwritten.
- Keep the canonical script and its tests in this repository, and publish a
  usable HTTPS source for [the install URL](/quest/m2/moq-install-url.md).
- Document install, upgrade, version selection, directory override, and
  removal in `doc/setup/install.md`.
- Wire installer tests into `just check` and CI: initial install, upgrade,
  explicit downgrade, product-specific latest selection, unsupported hosts,
  corrupt or missing assets, and a failure preserving the existing binary.
  Run the script against real release assets in a temporary directory and
  run the installed `moq --version`.

## Related

- [Binary release workflow](/quest/m1/tooling/release-binary.md) - reuse its
  artifacts without requiring workflow consolidation
- [`moq relay`](/quest/m2/moq-relay-subcommand.md) - relay functionality joins
  the same executable independently of its installation method
