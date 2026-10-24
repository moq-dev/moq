# [S] SRT tells a clean end from a failure

## Goal

An SRT ingest whose peer stops sending without a shutdown handshake ends its
broadcast with an error, while a peer that shuts down cleanly finishes it.
Today both finish (`rs/moq-srt/src/server.rs`, the `Ok(())` arm after the
socket stream ends), because srt-tokio ends the stream the same way for both.

## Plan

Decided with the maintainer on 2026-10-23: fix it at the source.

Found on 2026-10-24: srt-tokio 0.4.4 (latest, and upstream `main`) cannot
tell them apart. `SrtSocket`'s stream never yields `Err`; its task returns
`()`. In srt-protocol, a peer Shutdown packet, the peer idle timeout,
and a UDP or listener channel error all move the receiver to `Drain` and then
`Closed`, and the private `ConnectionStatus` keeps no cause. The task then
closes the data channel, so moq-srt sees `None` for all three. Statistics
carry no shutdown counter and stop updating at close.

What would fix it, by owner:

- Upstream: srt-protocol records why the receiver left `Open` (peer shutdown,
  idle timeout, socket closed, local close) and exposes it on
  `DuplexConnection`. srt-tokio then yields `Err` (`TimedOut` or
  `ConnectionAborted`) before `None` for any cause other than a peer
  shutdown or local close. The stream item is already `io::Result`, so no
  type changes; moq-srt's `try_next()?` would take the abort arm untouched.
  Upstream is unmaintained (see
  [SRT on the shared socket](/quest/m2/one-port/srt-demux.md)), and it is
  outside the moq-dev and kixelated orgs: ask before posting there.
- In moq-srt: drive sans-io srt-protocol directly, as
  [SRT on the shared socket](/quest/m2/one-port/srt-demux.md) already plans.
  The driver sees every inbound packet, so a connection that closes without
  a valid Shutdown control packet from the peer is a failure. It also takes
  `now` as input, which the mocked-clock tests need: srt-tokio reads std's
  clock directly, so `tokio::time::pause` cannot drive its idle timeout.

Open: which route, and whether this stays in m1 or moves under the m2
driver.

Tests either way, with a mocked clock: a silent peer aborts, a clean
shutdown finishes, in both the server and the `Client` pull
(`serve_publish`).

Public API: none. Wire: none.
