# [S] Linux relay packages ship io_uring

## Goal

The official Linux moq-relay builds (the `.deb`, `.rpm`, and tarballs from
`.github/workflows/moq-relay.yml`, and the Nix package in `nix/overlay.nix`)
are compiled with the `io-uring` feature, so `--runtime-io-uring` works on a
packaged relay. Today none of them enable it, yet the packaged systemd unit
sets `LimitMEMLOCK=infinity` for io_uring
([#4197](https://github.com/moq-dev/moq/pull/4197)): the unit prepares for a
runtime the binary cannot run. The runtime stays opt-in; this changes what
ships, not the default.

## Plan

- Decided: ship it, but only once the io_uring runtime is on par with the
  tokio one for what a packaged relay promises: every configured protocol
  served, the `[quic]` tuning honored, and closes delivered. Offering a flag
  that silently serves less than the default runtime would be worse than not
  offering it. Performance work is not a prerequisite.
- Enable the feature only on Linux targets. Confirm the zigbuild glibc 2.34
  build still links and that nothing new is needed at runtime.
- A kernel without the io_uring features the workers need must make
  `--runtime-io-uring` fail loud at startup with the reason, not fall back.
- Package smoke check in the release workflow: start the packaged binary
  with `--runtime-io-uring` on the runner, accept one session, and exit,
  failing the release if it cannot. Check it under the unit's limits too,
  since memlock is why #4197 touched the unit.
- Docs: `doc/bin/relay/` says packaged Linux builds include the runtime, how
  to turn it on, and the memlock note.

Public API: none. Wire: none.

## Required

- [moq-transport on io_uring](/quest/m1/uring-ietf.md) - a packaged relay
  must not drop protocols when the ring is on
- [Flow-control windows](/quest/m1/uring-flow-control-windows.md) - the
  `[quic]` section must not be refused at startup on the ring
- [Close before teardown](/quest/m1/quic/uring-close.md) - sessions on the
  ring must end with their application close
