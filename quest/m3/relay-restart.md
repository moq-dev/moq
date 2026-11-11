# [XL] moq-relay restarts without dropping sessions

## Goal

An operator upgrades a relay by starting the new process beside the old one
on the same UDP port. New sessions go to the new process. Existing sessions
stay with the old process until they end or its drain deadline passes. The
upgrade itself takes seconds, and a session that would have ended inside the
deadline is never cut.

## Plan

Decided 2026-11-09 (maintainer, `/quest-plan`): ✅ planned now, built after
downstream's batched rollouts (rejected: building it first; not building
it). m3, because a downstream deployment's rollouts wait on it: its relays
restart one at a time, and each restart waits minutes for sessions to drain,
so the drain sets the length of every rollout and every rollback.

Prior art, read 2026-11-09:

- Meta's Socket Takeover passes the UDP socket to the new process over a
  UNIX socket, and the new process forwards packets for old connections to
  the old process, keyed on the QUIC connection ID
  ([Zero Downtime Release, SIGCOMM 2020](https://par.nsf.gov/servlets/purl/10211855)).
- Cloudflare's udpgrm puts both processes in one `SO_REUSEPORT` group and
  routes with eBPF: new flows to the newest socket generation, existing
  flows to their owner, optionally by a generation encoded in the
  connection ID
  ([udpgrm](https://blog.cloudflare.com/quic-restarts-slow-problems-udpgrm-to-the-rescue)).
- GOAWAY with a new URI, which moq-lite already carries, moves a client that
  can reconnect. It stays the path for a session that outlives the deadline.

Guidance:

- Say how a new handshake reaches the new process. Nothing does today: a
  client's Initial carries a CID it invented, `moq_sock::shard` steers only
  by server-issued CIDs and binds its group once, QUIC-LB forwarding never
  forwards long headers, and the io_uring backend refuses `listen.lb.id`
  because shard steering and QUIC-LB compete for the CID bytes. Choose
  between inheriting the sockets by fd passing, an eBPF steer as in udpgrm,
  or reusing the shard index or QUIC-LB server ID as the generation; do not
  add a third CID layout. Say whether `reset.secret` stays. Cover both
  backends, `moq-tokio` and `moq-uring`.
- Decide what happens to cluster peer sessions: handed to the new process,
  or re-dialed. Today a peer session does not leave on GOAWAY and holds a
  drain to its bound. Measured on a staging mesh: a relay with no inbound
  peer stopped about two minutes after its stop signal, and every relay
  with one stopped about seven minutes after it, having waited out the full
  300 s session window that opened at the two-minute mark. So inbound peers
  add about five minutes to each restart and to each wave of a batched
  rollout.
  Decided 2026-11-11 (maintainer, `/quest-plan`): ✅ remove that cost
  here, by running the new process beside the old (rejected as an interim:
  the draining relay closing inbound peer sessions once its other sessions
  have left, a forced close that works against this design; a separate
  quest for it).
- Two processes share one node identity for the length of a drain. Stats,
  routes and announcements must count and name each session once across
  them.
- Memory and file descriptors double during the overlap; measure it.
- Linux only. Integrate with systemd so the unit's restart is the upgrade.
- A crash of either process must not take the other's sessions with it.

Public API: relay configuration for the overlap and its deadline, additive.
Wire: none expected; the connection ID's layout is local to a relay.

## Related

- [GOAWAY migration follow-ups](/quest/m3/goaway-migration-followups.md) - the client's side of leaving a draining session
