# [M] One-command moq installation and upgrades

## Goal

A canonical Bash installer installs the released `moq` binary on supported macOS and Linux machines without Rust or sudo. Running
it again upgrades the same installation; selecting a version supports
reproducible installs and deliberate downgrades.

Install only `moq` from the `moq-cli` release. Token and relay functionality
belong to its subcommands, not separate installer choices. This work does
not implement those subcommands, automatic updates, a self-update command,
service setup, Windows support, or new release targets.

## Plan

- Default to the latest stable `moq-cli` release, with an explicit version
  option. Resolve that product's tags, not the repository-wide latest
  release: this repository publishes multiple independently versioned crates.
  Refuse missing versions, malformed input, and incomplete releases clearly.
- Reuse the release archives and `SHA256SUMS`. Verify the selected archive
  before extracting and installing its expected executable. Stage and check
  the replacement before an atomic replacement on the destination filesystem;
  download, verification, extraction, or validation failures leave an existing
  installation usable. Clean up temporary files on failure or interruption.
- Support the existing targets: macOS ARM64 and Linux x86_64/ARM64 with
  glibc 2.34 or newer. Refuse unsupported operating systems, architectures,
  and libc variants with actionable diagnostics. Intel macOS and musl/Alpine
  require separate release work.
- Default to `~/.local/bin` with an explicit directory override. Do not invoke
  sudo or edit shell profiles. Print the installed version and path, and
  shell-appropriate PATH instructions when needed. Detect when another
  `moq` on PATH would take precedence so success does not imply the wrong
  binary will run. Do not follow an existing destination symlink into a
  package manager's installation or overwrite a conflicting unmanaged file.
  Repeated installs must recognize and replace their own installation.
- Keep the canonical script and its tests in this repository. Publish a
  usable HTTPS source for the dependent website quest; that quest exposes
  `https://moq.dev/install.sh` without duplicating installer logic.
- Document first install, latest-version upgrade, explicit version selection,
  directory override, PATH setup, and removal in `doc/setup/install.md`.
  Use the working canonical URL until the website quest switches the example.
  Package-manager installations continue to use their package manager for
  upgrades. Describe only the subcommands the selected release actually ships.
- Wire installer tests into `just check` or `just test` and CI. Cover initial
  install, repeat install, upgrade, explicit downgrade, product-specific latest
  selection, unsupported hosts, corrupt/missing assets, destination conflicts,
  and failure preserving an existing executable. Use controlled fixtures for
  failure cases and native macOS/Linux smoke coverage for executable startup.
  Exercise the canonical script with real release assets in a temporary
  install directory and run the installed `moq --version`. The dependent
  website quest owns verification of the final public URL.

## Related

- [Binary release workflow](/quest/m2/tooling/release-binary.md) - reuse its
  artifacts without requiring workflow consolidation
- [`moq relay`](/quest/m2/moq-relay-subcommand.md) - relay functionality joins
  the same executable independently of its installation method
- [Install URL](/quest/m2/moq-install-url.md) - exposes this installer through
  the moq.dev website after it is published
