# [M] moq-relay in nixpkgs

## Goal

`nix-shell -p moq-relay` (and `pkgs.moq-relay` on NixOS) installs the
released relay from upstream nixpkgs, with kixelated as maintainer, and the
nixpkgs-update bot keeps it current from our `moq-relay-v*` tags.
`doc/setup/install.md` gives the nixpkgs command once the package reaches
nixpkgs-unstable.

Out of scope: upstreaming the `services.moq-relay` NixOS module, moq-gst,
and moq-bench. Our flake, overlay, and nix.moq.dev cache stay as they are for
`main` and `release` builds. The CLI follows in
[moq-cli in nixpkgs](/quest/m3/nixpkgs-cli.md).

## Plan

Decided 2026-11-05 (maintainer, /quest-plan):

- Which of our outputs become nixpkgs packages?
  - ✅ moq-relay + moq-cli
  - NixOS module
  - moq-gst plugin
  - moq-bench
- Who submits the nixpkgs PR?
  - ✅ Agent preps, you open
  - Agent opens after your review
  - You write it by hand
- Relay first or with the CLI? ✅ (maintainer's words) "moq-relay as a
  standalone first, moq-cli only when we're happy with the feature set"

nixpkgs requires a responsible human to review any AI-assisted contribution
before it is submitted, with an `Assisted-by:` commit trailer naming the tool
and model (`CONTRIBUTING.md`, "Automation/AI policy"). That is why the
maintainer opens the PR rather than an agent.

Two stages, each its own PR here:

1. Prepare. Use the existing kixelated/nixpkgs fork (it exists as of
   2026-11-12, with no moq-relay branch yet) and push a branch with two commits: kixelated's
   `maintainers/maintainer-list.nix` entry (GitHub `kixelated`, id 432854),
   then `pkgs/by-name/mo/moq-relay/package.nix`. Both commits carry
   `Assisted-by:`. Package from the latest `moq-relay-v*` tag with
   `fetchFromGitHub`, `rustPlatform.buildRustPackage`, `-p moq-relay`, and
   the `jemalloc` feature our overlay builds (`nix/overlay.nix`,
   `moqRelayArgs`). Set `passthru.updateScript` to `nix-update-script` with
   `--version-regex 'moq-relay-v(.*)'`, since the monorepo tags every crate.
   Run whatever tests pass inside the sandbox rather than disabling them all.
   Prove it with `nix-build -A moq-relay`, the by-name and nixfmt checks
   nixpkgs CI runs, `moq-relay --help`, and the update script run once.
   Then this quest's PR rewrites the quest to the waiting state: a
   `Maintainer:` item to review the branch and open the nixpkgs PR (agents
   cannot be the accountable submitter), followed by `NixOS/nixpkgs:` merge
   and channel items.
2. Finish. Once `moq-relay` reaches nixpkgs-unstable, add it to
   `doc/setup/install.md` beside the flake commands, and delete this quest.

The nixpkgs `moq` attribute is an unrelated Go mocking tool
([matryer/moq](https://github.com/matryer/moq)), which only matters for the
CLI.

## Related

- [moq-cli in nixpkgs](/quest/m3/nixpkgs-cli.md) - the CLI follows once its release feature set lands
