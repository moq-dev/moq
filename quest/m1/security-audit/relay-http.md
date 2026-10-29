# [XS] Validate relay HTTP surface

## Goal

The relay's HTTP `/fetch` response cannot be sniffed as active content on the
relay origin or stored by a shared cache, and the internal ops listener refuses
requests carrying `Origin` or a foreign `Host`/URI authority.

## Plan

The headers and internal-listener policy are implemented in
[moq-dev/moq#7210](https://github.com/moq-dev/moq/pull/7210), including IPv4,
IPv6, wildcard binds, HTTP/2, and embedded routes. No API or policy decision
remains. Finish the full local gate, address any findings, then remove this
quest and mark that PR ready for review.

The selected 679 tests, 550 io_uring tests, relay all-feature compile, and
tooling tests passed. A full gate remains incomplete: this runner's inherited
`BASH_ENV` reloads the dev shell and overrides fixture PATHs; after disabling
it, the shared compiler scheduler and host admission queue stalled further
validation. The queued runs were stopped before handoff.

In an already-loaded Nix dev shell, run
`env -u BASH_ENV MBX_DISABLE=1 quest gate just ci` on an available host.

Public API: none. MoQ wire: none. HTTP response headers and the documented
internal authority policy change as described in the PR.
