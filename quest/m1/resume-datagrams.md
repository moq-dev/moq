# [S] A resumed track skips only datagrams it already delivered

## Goal

After a same-epoch route swap, a resumed track drops a datagram from the new
copy only if the reader already handed out that same datagram. A datagram
first emitted after the swap is delivered even when its sequence is below the
newest one handed out, since `insertDatagram` allows out-of-order sequences and
`recvDatagram()` delivers in arrival order.

## Plan

Found by a hosted Codex review of #5836. Each copy records the newest datagram
the reader handed out before that copy subscribed (JS `Copy.datagramsSeen` in
`js/net/src/resume.ts`, Rust `datagrams_seen` in
`rs/moq-net/src/model/resume.rs`), and JS `Reader.tryDatagram` and Rust
`Reader::poll_datagram_once` drop everything at or below it as a replay. A relay forwarding datagram 5 after 10
therefore loses 5 on the new route, though the old route would have delivered
it.

- Skip by identity: record which datagram sequences the reader delivered, as
  `Delivered` does for groups, and drop only those. Prune the record with the
  reader's budget so it stays bounded, rather than with a count cap.
- Apply the same rule in both languages, so the JS and Rust readers agree.
- Regression test in both: route A delivers datagram 10, route B takes over and
  delivers a replay of 10 and a new 5. The reader gets 5 once, and 10 only once.

Public API: behavior only. Wire: none.

## Related

- [Resumed in-flight groups](/quest/m1/resume-in-flight.md) - the group side of what a replaced copy still delivers
