# [XS] The Linux error-queue reader decodes truncated control data

## Goal

moq-sock's `MSG_ERRQUEUE` reader (`rs/moq-sock/src/udp/linux.rs`) handles `MSG_CTRUNC` the way the datagram receive paths do: it decodes whatever control messages fit and warns once, instead of returning an `InvalidData` error for the whole entry.

## Plan

Found by [moq-dev/moq#7543](https://github.com/moq-dev/moq/pull/7543), which made the Windows receive path keep a datagram with truncated control data, matching what [moq-dev/moq#7394](https://github.com/moq-dev/moq/pull/7394) did for `recvmmsg`, `recvmsg`, and Apple's `recvmsg_x`. The error-queue reader is a different path and still turns `MSG_CTRUNC` into `io::ErrorKind::InvalidData` (about line 55). It only matters if error-queue metadata ever grows past `ERR_CMSG_LEN`, so this is defensive.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, matching the receive paths. Walk the cmsgs that fit (the iterator is already bounded by the returned control length), return the decoded entry if one is complete, and warn once through a `Once` as `decode_recv` in `unix.rs` does. Extend the existing mocked `MSG_ERRQUEUE` test (`LinuxError::decode`, about line 309) with a truncated case.

Public API: none. Wire: none.

## Related

- [Wire fuzz coverage](/quest/m2/wire-fuzz-coverage.md) - fuzzes the cmsg walks this path shares
