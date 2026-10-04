# [M] JS close drains served subscriptions

## Goal

`Connection.close()` in `@moq/net` waits for the requests it serves to
finish before closing, as Rust's `Session::close()` does, within the same
one-second deadline. On every lite version it waits for each served SUBSCRIBE
to deliver its group streams, and for served FETCH and TRACK requests
(lite-05+); on lite-07 a served SUBSCRIBE also waits for the subscriber's FIN
or reset on the Subscribe Stream. Today `close()` waits only for announce
withdrawals, so a JS publisher that finishes a track and closes can cut the
final group short.

## Plan

- Full parity with Rust, not lite-07 alone (decided 2026-10-04): the FIN wait
  means nothing while `close()` skips served subscriptions entirely.
- Mirror Rust's shape: count served SUBSCRIBE, FETCH, and TRACK requests from
  dispatch until completion or cancellation (`owed` in
  `rs/moq-net/src/lite/publisher.rs`), and gate the lite-07 wait, SUBSCRIBE
  only, on the same version check as Rust's
  `Version::waits_for_subscriber_fin()` (#4628). JS subscribers already FIN
  on lite-07 after settling the track tail.
- Keep `abort()` immediate and the deadline at one second.
- Regressions with mocked time: a finished track's final group arrives before
  close on lite-05/06/07, a lite-07 subscriber withholding FIN holds close
  until the deadline, and abort during the drain ends at once.
- Update the `close()` doc comment in `js/net/src/lite/connection.ts` and the
  close paragraph in `doc/lib/js/net.md`.

Public API: none (behavior of `close()`). Wire: none; lite-07 FIN already
specified.

## Related

- [Track tail interop](/quest/m1/track-tail-interop.md) - proves the close across languages through a relay, which this makes hold for JS publishers
