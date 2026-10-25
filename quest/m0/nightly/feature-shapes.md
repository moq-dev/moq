# [S] Feature shapes in the gate

## Goal

The selected Rust feature shapes pass the complete local PR gate on a
supported host, so feature-gated dead code fails the PR that adds it.

## Plan

[moq-dev/moq#6703](https://github.com/moq-dev/moq/pull/6703) adds selected
feature extremes and isolated net, QUIC, transport, and media boundaries to
the gate, fixes the missing helper gates and minimal-model documentation it
exposed, and leaves the exhaustive targets and optimized artifacts nightly.

Finish validation with `just ci` on a host that permits the runner image's
user namespace setup. The local attempt fails in an unchanged Nix image build
at `unshare: write failed /proc/self/uid_map: Operation not permitted`, which
cancels the remaining module checks. The focused gate block and selector
contracts can run independently of that image build.

Public API: none. Wire: none.
