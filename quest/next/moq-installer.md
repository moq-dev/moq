# [M] One-command moq installation and upgrades

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

- Default to the latest stable `moq-cli` release, with an explicit version
  option. Resolve that product's tags, not the repository-wide latest
  release: this repository publishes multiple independently versioned crates.
  Refuse missing versions, malformed input, and incomplete releases clearly.
- Reuse the release archives and `SHA256SUMS`. Verify the selected archive
  before extracting and installing its expected executable. Stage and check
  the replacement before modifying the destination. Commit the executable and
  ownership record as one recoverable transaction on the destination
  filesystem: stage the new pair, retain the validated prior pair, and write a
  durable journal before either rename. Record distinct phases after the
  executable rename and after the ownership-record rename. On a handled
  failure, roll back both files. After interruption, the next run must use the
  journal to complete the new pair when both staged objects validate together.
  If both renames completed, validate the installed pair and finish cleanup;
  otherwise restore the prior pair, or remove every transaction file for an
  initial install. Recovery must not misclassify a partial transaction as an
  unmanaged installation.
  Flush staged files, journal updates, renames, and their directory entries at
  the required commit boundaries. Remove the journal and backups only after the
  matching pair is durable. This is the atomic installation contract: recovery
  exposes either the complete old pair or the complete new pair, never a mixed
  pair. A failed initial install leaves no destination, while a failed upgrade
  leaves the prior executable and ownership record usable. Clean up temporary
  files after commit or rollback.
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
  Keep a durable ownership record bound to the destination and installed
  binary digest. Refuse replacement when the record is missing, malformed,
  or mismatched, including a record copied from another destination. A valid
  prior installation can be replaced; failed upgrades must preserve both its
  binary and usable ownership record.
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
  failure cases, including interruption before and after every journal, rename,
  durability, and cleanup boundary. Explicitly cover the post-second-rename,
  pre-cleanup state. Assert that each case completes the new pair or restores
  the old pair, and that an initial-install failure leaves neither file. Add
  native macOS/Linux smoke coverage for executable startup.
  Exercise the canonical script with real release assets in a temporary
  install directory and run the installed `moq --version`. The dependent
  website quest owns verification of the final public URL.

## Related

- [Binary release workflow](/quest/next/tooling/release-binary.md) - reuse its
  artifacts without requiring workflow consolidation
- [`moq relay`](/quest/next/moq-relay-subcommand.md) - relay functionality joins
  the same executable independently of its installation method
- [Install URL](/quest/next/moq-install-url.md) - exposes this installer through
  the moq.dev website after it is published
