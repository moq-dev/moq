# [S] iroh opt-in for moq-relay

## Goal

moq-relay no longer builds iroh by default, and its shipped binaries, nix
package, and Docker image leave it out. iroh adds about 63 crates to the
relay: 401 dependencies with it, 338 without.

## Plan

Decided in planning:

- Only the relay changes. moq-cli keeps iroh by default because the P2P
  questline dials native peers over it, which mDNS's LAN discovery doesn't
  replace. `moq relay` (the planned verb) keeps iroh through moq-cli's
  features.
- Target `dev`. Removing a default feature from a published crate, and
  flags from a shipped binary, is a published break.

Guidance:

- An iroh setting given to a build without the feature must be refused, not
  ignored, whether it comes as a flag, an environment variable, or TOML.
- Update `doc/bin/relay/` and any example that relies on the relay's iroh
  listener. Report the binary size difference in the PR.

## Related

- [`moq relay`](/quest/m1/moq-relay-subcommand.md) - forwards the relay's features from moq-cli's
- [P2P](/quest/m1/p2p/README.md) - why moq-cli keeps iroh
