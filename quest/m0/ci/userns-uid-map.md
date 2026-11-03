# [XS] The flake lane skips hosts that can't map user namespaces

## Goal

`check flake` skips with a clear note, instead of failing, on a host whose
sandbox can't write `/proc/self/uid_map`, as it already does when user
namespaces are unavailable.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7461](https://github.com/moq-dev/moq/pull/7461), whose
local gate failed `check flake` building nixpkgs' `github-runner` with
`unshare: write failed /proc/self/uid_map`. `MOQ_USERNS_UNAVAILABLE` in
`sh/dispatch.sh` does not catch this case; probe the write itself. CI hosts
that can map namespaces must still run the lane.
