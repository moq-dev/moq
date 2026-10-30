# [XS] VM checks refuse a host without KVM

## Goal

`just ci`'s `check flake` fails at once with a message naming the missing
`/dev/kvm` when it would run the NixOS VM tests (such as
`vm-test-run-github-runner-vm`) on a host without KVM, instead of booting the
VMs under software emulation until they time out with "Shell did not start
in time".

## Plan

Decided by the maintainer on 2026-10-29, from the follow-ups of
[moq-dev/moq#7175](https://github.com/moq-dev/moq/pull/7175), whose local gate
spent about 19 minutes before failing this way on a host without KVM.

- Refuse, don't skip: a skipped check would let a local gate read as passing.
  The message says the check needs KVM and that CI runs it.
- The other follow-up from #7175, a nested Codex shell's `BASH_ENV` snapshot
  inflating `PATH` past mbx's exec limit, did not reproduce with codex-cli
  0.162.1 (`BASH_ENV` unset, `PATH` stable across nested shells), so it is
  neither planned nor reported.
