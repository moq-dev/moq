# [XS] moq-sock's control buffer is sized for the send options too

## Goal

`cmsg::LEN` in moq-sock is derived from, or checked at compile time
against, both the receive options and the send options (ECN, the GSO
segment size, and the source-address pktinfo) on every Unix target, so a
send cmsg that outgrows the buffer fails the build instead of truncating.
Apple's IPv4 arm is sized for the `in_pktinfo` it now sends.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7394](https://github.com/moq-dev/moq/pull/7394)
and [moq-dev/moq#7418](https://github.com/moq-dev/moq/pull/7418).

[moq-dev/moq#7394](https://github.com/moq-dev/moq/pull/7394) makes `cmsg::LEN` (`rs/moq-sock/src/udp/cmsg/unix.rs`, 96 on main
in `cmsg/mod.rs:224`) a compile-time `cmsg_space` sum over each target's
receive set and drops the runtime subset assert in `UdpSocketState::new`
(`rs/moq-sock/src/udp/unix.rs:60` on main). It notes the send set fits but
nothing checks it. [moq-dev/moq#7418](https://github.com/moq-dev/moq/pull/7418) switches Apple's IPv4 source to `IP_PKTINFO` with
`in_pktinfo`, while [moq-dev/moq#7394](https://github.com/moq-dev/moq/pull/7394) sized Apple's IPv4 arm for `in_addr`; whichever
lands second should move `apple` to the `in_pktinfo` arm, so check that
happened and do it here if not.

Take the larger of the receive and send sums per family, or add a
`const` assert that the send sum fits; prefer the max, which needs no
reasoning about which set is bigger. Drop `encode_src_ip` on Apple if it
is still unused. Verify with cross-target `cargo check`
(macOS, FreeBSD, Linux) or the `platform.yml` workflow, and watch the next
macOS Nightly's `ipv4_source_address_override` (and `fast-apple-datapath`).
The Windows receive path is [its own quest](/quest/m2/cmsg-windows-truncation.md).

Public API: none. Wire: none.

## Required

- [Bound cmsg payload reads and keep every receive cmsg](/quest/m2/cmsg-decode-bound.md) - its PR, [moq-dev/moq#7394](https://github.com/moq-dev/moq/pull/7394), adds the compile-time sizing this extends
