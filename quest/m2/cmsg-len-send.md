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

[moq-dev/moq#7394](https://github.com/moq-dev/moq/pull/7394) (merged) made `cmsg::LEN` (`rs/moq-sock/src/udp/cmsg/unix.rs:72`)
a compile-time `cmsg_space` sum over each target's receive set and dropped
the runtime subset assert in `UdpSocketState::new`. Its doc comment says the
sum covers the send set, but nothing checks it. [moq-dev/moq#7418](https://github.com/moq-dev/moq/pull/7418) (landed as
[48aaca7d50](https://github.com/moq-dev/moq/commit/48aaca7d50023ce80308487e69b84d419bbc17f3))
switched Apple's IPv4 source to `IP_PKTINFO` with `in_pktinfo`
(`rs/moq-sock/src/udp/unix.rs:686`), but `LEN` still sizes Apple's IPv4 arm
for `in_addr` (`cmsg/unix.rs:78`). Move `apple` to the `in_pktinfo` arm here.

Take the larger of the receive and send sums per family, or add a
`const` assert that the send sum fits; prefer the max, which needs no
reasoning about which set is bigger. Drop `encode_src_ip` on Apple if it
is still unused. Verify with cross-target `cargo check`
(macOS, FreeBSD, Linux) or the `platform.yml` workflow, and watch the next
macOS Nightly's `ipv4_source_address_override` (and `fast-apple-datapath`).
The Windows receive path is [its own quest](/quest/m2/cmsg-windows-truncation.md).

Public API: none. Wire: none.
