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
- Put the whole script in a function called on the last line, like rustup,
  so a download cut off mid-pipe runs nothing.
- Download the archives `.github/workflows/release-binary.yml` already
  publishes; the installer needs no workflow change.
- Default to the `moq-cli` version baked in when the worker was deployed,
  with an explicit version option that builds the download URL from the
  `moq-cli-v<version>` tag. The script makes no GitHub API calls: those are
  limited to 60 per hour unauthenticated, which CI runners and shared NAT
  exceed, and the repository-wide latest release is often another crate.
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
  `infra/moq-sh/**`, and again after `release-binary.yml` finishes publishing
  a `moq-cli-v*` release, baking that release in as the default version.
  So moq.sh always serves the released script and the newest `moq-cli`.
  PRs touching it run `wrangler deploy --dry-run`. Follow `release-js.yml`'s
  concurrency split. Add `just infra moq-sh deploy` for manual use, and
  include it in the aggregate `just infra deploy` and `infra/README.md`.
- CI deploys with the `CLOUDFLARE_API_TOKEN` Actions secret, which exists.
  It has the account-wide Workers Editor role (Workers Scripts Edit is
  legacy), so it can deploy every worker in the account, and no
  Zone > Workers Routes access: Editor deploys new versions as long as a
  deploy does not add, change, or remove a route or custom domain.
- Editor cannot create a worker, so the first `just infra moq-sh deploy`
  runs under the maintainer's wrangler login, creating the worker and its
  `moq.sh` custom domain. An agent without that login stops once
  `--dry-run` passes and hands the deploy to the maintainer. A later domain
  change is deployed by hand too.

### Verification and docs

- Wire installer tests into `just check` and CI: initial install, upgrade,
  explicit downgrade, product-specific latest selection, unsupported hosts,
  corrupt or missing assets, and a failure preserving the existing binary.
  Run the script against real release assets in a temporary directory and
  run the installed `moq --version`. Run the tests under dash and on a macOS
  runner (via `platform.yml`), since macOS has `shasum -a 256` rather than
  `sha256sum` and BSD `mktemp` and `tar`.
- After deploying, run `curl -fsSL https://moq.sh | sh -s -- <tmp dir option>`
  and confirm `moq --version`.
- Give the workflow `workflow_dispatch` and run it once after the manual
  deploy, so the CI token is proven before the first release needs it.
  Cloudflare says custom domains do not support per-Worker roles yet; if
  the token is rejected on the custom domain, ask the maintainer to add
  Zone > Workers Routes > Edit on `moq.sh`.
- Document install, upgrade, version selection, directory override, and
  removal in `doc/setup/install.md`, leading with the one-liner. Add the
  worker to the table in `infra/README.md`.

## Related

- [Ship capture and playback](/quest/m1/cli-packaging.md) - decides what the
  released binary this installs can do
- [`moq relay`](/quest/m3/moq-relay-subcommand.md) - relay functionality joins
  the same executable independently of its installation method
