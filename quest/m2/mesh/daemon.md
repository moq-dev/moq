# [L] Daemon

## Goal

`moq mesh up` enrolls the host and runs a localhost relay that dials its mesh
relays and discovers LAN peers. `moq mesh serve ssh tcp://localhost:22`
publishes a service, and `ssh -o ProxyCommand='moq mesh pipe %h ssh' <id>`
reaches it from another host by endpoint ID, taking the LAN path when both
hosts share a network. `moq mesh install` runs it as a system service by default
(a systemd system unit or a launchd daemon), with `--user` for a per-user
unit that warns about macOS's Local Network prompt.

## Plan

See the [epic](/quest/m2/mesh/README.md) for the decisions; this quest proves
the epic's first shippable outcome.

- Extend the `moq` CLI; reuse its relay-node flags rather than a second config
  surface. Confirm the verbs against `--help` before coding.
- Mesh relays are `upstream` links; LAN peers come from `[cluster.lan]` mDNS
  and rank above them. A LAN peer is admitted only if it proves mesh
  membership with its own enrollment; no extra shared secret for the user to
  configure.
- Nothing is served unless `serve` names it. A restart keeps the endpoint key
  and enrollment, so a node never re-enrolls on its own.
- Decided 2026-10-31 (maintainer): install defaults to a system service
  because macOS exempts launchd daemons and root from Local Network privacy
  but prompts launchd agents, and a denied prompt silently breaks LAN
  discovery.
- Update `doc/bin/cli.md` inline.

## Required

- [Outbound federation](/quest/m1/cluster-routing/federation.md) - upstream links and LAN preference
- [Enrollment](/quest/m2/mesh/enroll.md) - the relay URLs and token the daemon runs on
- [Endpoint API](/quest/m2/mesh/endpoint.md) - the streams `serve` and `pipe` use
