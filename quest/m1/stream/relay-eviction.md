# [M] Lossless relay hops for byte streams

## Goal

A `tcp` socket session carries a bulk transfer through a relay without loss,
however fast the writer: every hop paces its ingest to its fastest reader, so
no reader of the session's one group ever ends `Evicted`.

## Plan

Found while validating `moq tcp` (the Rust byte streams quest): a 300 MB HTTP
download through `moq tcp dial`, a local `moq-relay`, and `moq tcp accept`,
all on one host, ends with a reset partway in about half the runs. It happens
over both WebTransport and native QUIC (`moql://`), more often with 8 KiB
frames than 256 KiB ones.

The relay's ingest of the accepting side's half aborts its group with
`evicted` (its subscriber logs `group error err=evicted group=0`) while the
accepting side's publisher only sees the relay cancel, so the eviction is the
relay's own copy shedding a frame its one reader still needed. The endpoints'
local writers wait on `group::Producer::poll_ready`, so they cannot be the
cause. Suspects, in order: ingest skipping its pace when
`transport::boxed::RecvStream::pause` refuses
(`coding::Reader::poll_pace` reads on when it cannot pause), the paused-credit
budget being half the connection window (4 MiB by default) against a 1 MiB
stream window, and a second, faster reader of the relay's copy that pacing
waits on instead.

Reproduce with a relay config accepting `moq-lite-07-wip`, then
`moq --connect <relay> tcp accept web --local <http server>` and
`moq --connect <relay> tcp dial web --local 127.0.0.1:28081`, and `curl` a
few hundred MB through `127.0.0.1:28081` in a loop. Add a sim test of the
same shape (a fast local writer, a relay, a reader paced by its consumer) that
fails first, then fix it at the root.
