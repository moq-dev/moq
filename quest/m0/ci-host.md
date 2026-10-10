# [XS] The CI host is online

## Goal

Condition: the maintainer's spare desktop (x86, RTX 3070 Ti) runs NixOS with
the NVIDIA driver, imports the `ci/runner/` module from
[Self-hosted CI](/quest/m0/self-hosted-ci.md), and has registered its
`moq-ci` and `moq-gpu` runners through the dedicated GitHub App. This is a
maintainer action: it needs admin access to the org and the host.

Check: `gh api orgs/moq-dev/actions/runners` lists four `moq-ci` runners and
one `moq-gpu` runner online, and `gh variable get CI_RUNNER --repo moq-dev/moq`
prints `moq-ci`. Then a same-repo PR's Check and Test run on `moq-ci` with no
free-disk-space, Nix install, or hosted cache restore step, and their logs
show the `rust-cache` action in self-hosted mode with mbx hitting the local
server, while fork and Dependabot PRs run on `ubuntu-24.04-arm`. Record queue and run times against the 2026-10-09
baseline (20-50 min queued, 4-18 min run) in the PR that deletes this quest.

Advance it now: install NixOS with the NVIDIA driver, and create the GitHub
App (org "Self-hosted runners" permission only) and a runner group restricted
to `moq-dev/moq`. Once Self-hosted CI merges, follow `ci/runner/README.md` to
import the module, then
`gh variable set CI_RUNNER --repo moq-dev/moq --body moq-ci`. Unset the
variable to fall back to hosted runners whenever the host is down.

When the check holds, delete this quest and every `Required` entry that links
it.

## Related

- [Self-hosted CI](/quest/m0/self-hosted-ci.md) - the module and routing this host runs
