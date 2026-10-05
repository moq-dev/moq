# [M] Install moq with curl -fsSL https://moq.sh | sh

## Goal

`curl -fsSL https://moq.sh | sh` installs the released `moq` binary on
supported macOS and Linux machines without Rust or sudo. Running it again
upgrades the same installation; selecting a version supports reproducible
installs and deliberate downgrades.

Install only `moq` from the `moq-cli` release. Token and relay functionality
belong to its subcommands, not separate installer choices. This work does
not implement those subcommands, automatic updates, a self-update command,
service setup, Windows support, or new release targets.

## Plan

Promoted to m1 on 2026-10-05: the project owns `moq.sh`, which names the
consumer. This merges the former installer and moq.dev install-URL quests,
since the script and its hosting now both live in this repository.

### Script

- POSIX sh, not Bash, so `| sh` works with dash and busybox. Check it with
  shellcheck in POSIX mode.
- Live at `infra/moq-sh/install.sh`.
- Download the archives `.github/workflows/release-binary.yml` already
  publishes; the installer needs no workflow change.
- Default to the latest stable `moq-cli` release, with an explicit version
  option. Resolve that product's tags, not the repository-wide latest
  release: this repository publishes multiple independently versioned crates.
  Refuse missing versions, malformed input, and incomplete releases clearly.
- Verify the selected archive against the release's `SHA256SUMS`, extract it
  into a temporary file in the destination directory, then atomically rename
  it over the destination. A failed install leaves the prior binary untouched.
  No journaled transaction or ownership record; that is out of proportion
  for one binary.
- Support the existing targets: macOS ARM64 and Linux x86_64/ARM64 with
  glibc 2.34 or newer. Refuse unsupported operating systems, architectures,
  and libc variants with actionable diagnostics.
- Default to `~/.local/bin` with an explicit directory override. Do not invoke
  sudo or edit shell profiles. Print the installed version and path, and PATH
  instructions when needed. Warn when another `moq` on PATH takes precedence.
  Refuse a destination that is a symlink, so a package manager's install is
  never overwritten.
- Options pass through the pipe as `sh -s -- <args>`; document that form.

### Hosting

- A new `infra/moq-sh` Cloudflare Worker, following `infra/apt` and
  `infra/rpm`, with a `custom_domain` route on `moq.sh`. The zone is already
  in the account.
- Compile the script into the worker as text. Every path and every client,
  browsers included, gets the script as `text/plain`, like sh.rustup.rs: no
  user-agent sniffing, so readers can audit what they pipe.
- A new workflow deploys the worker on push to `release` touching
  `infra/moq-sh/**`, so moq.sh always serves the released script. PRs touching
  it run `wrangler deploy --dry-run`. Follow `release-js.yml`'s concurrency
  split. Add `just infra moq-sh deploy` for manual use.
- CI deploy needs [the Cloudflare secret](/quest/m1/moq-sh-secret.md). This
  quest does not wait for it: the implementing agent may deploy once by hand
  with `just infra moq-sh deploy` to verify the public URL.

### Verification and docs

- Wire installer tests into `just check` and CI: initial install, upgrade,
  explicit downgrade, product-specific latest selection, unsupported hosts,
  corrupt or missing assets, and a failure preserving the existing binary.
  Run the script against real release assets in a temporary directory and
  run the installed `moq --version`.
- After deploying, run `curl -fsSL https://moq.sh | sh -s -- <tmp dir option>`
  and confirm `moq --version`.
- Document install, upgrade, version selection, directory override, and
  removal in `doc/setup/install.md`, leading with the one-liner. Add the
  worker to the table in `infra/README.md`.

## Related

- [CI secret](/quest/m1/moq-sh-secret.md) - lets the release workflow deploy
  the worker
- [Ship capture and playback](/quest/m1/cli-packaging.md) - decides what the
  released binary this installs can do
- [`moq relay`](/quest/m2/moq-relay-subcommand.md) - relay functionality joins
  the same executable independently of its installation method
