# [XS] moq-shaper's own socket never passes for path loss

## Goal

moq-shaper's sockets are sized like the relay's, so they don't drop at the rates drills run (480 Mbit/s in the socket-queues survey), and the shaper reports any drops at its own socket beside its path drops, so a drill that reads loss off the shaper attributes every drop correctly.

## Plan

Found by [moq-dev/moq#7529](https://github.com/moq-dev/moq/pull/7529)'s UDP queue-loss survey: moq-shaper binds its listen socket (`UdpSocket::bind` in `rs/moq-shaper/src/lib.rs`, `Shaper::bind`) at the kernel's 208 KiB default. Once in a shaped run it dropped 10 datagrams at its own socket, which showed as unattributed, since the shaper reports only its queue overflows; a drill would read them as path loss. #7529 also measured the relay's 8 MiB buffers absorbing a 100 ms stall with no drops, where 208 KiB dropped about 13% under 20 ms stalls.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2. Do both halves, since they're each small: give the shaper's sockets the relay's buffer sizing (moq-sock's 8 MiB `UDP_BUFFER` sizing in `rs/moq-sock/src/bind.rs`; reuse it rather than a second constant), and count its own socket's `sk_drops` (`SO_MEMINFO`, as #7529's survey read it) into the shaper's stats so drills report them separately.

Optional, only if it stays small: export per-socket `sk_drops` on the relay's `/metrics`. #7529 found nothing in production that needed it, so skip it if it needs new plumbing through both runtimes.

Public API: a socket-drop counter in moq-shaper's stats. Wire: none.
