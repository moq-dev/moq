# [M] JS close drains served subscriptions

## Goal

`Connection.close()` in `@moq/net` waits for the subscriptions it serves to
finish before closing, as Rust's `Session::close()` does, within the same
one-second deadline. On lite-05 and lite-06 it waits for each served
SUBSCRIBE's group streams to finish; on lite-07 it also waits for the
subscriber's FIN or reset on the Subscribe Stream. Today `close()` waits only
for announce withdrawals, so a JS publisher that finishes a track and closes
can cut the final group short.

## Plan

- Full parity with Rust, not lite-07 alone (decided 2026-10-04): the FIN wait
  means nothing while `close()` skips served subscriptions entirely.
- Mirror Rust's shape: count served SUBSCRIBEs from dispatch until completion
  or cancellation (Rust's `owed` in `lite::Publisher`), and gate the lite-07
  wait on the same private version check as Rust's
  `Version::waits_for_subscriber_fin()` (#4628). JS subscribers already FIN
  after settling the track tail.
- Keep `abort()` immediate and the deadline at one second.
- Regressions with mocked time: a finished track's final group arrives before
  close on lite-05/06/07, a lite-07 subscriber withholding FIN holds close
  until the deadline, and abort during the drain ends at once.
- Update the `close()` doc comment and `doc/lib/js` where it describes close.

Public API: none (behavior of `close()`). Wire: none; lite-07 FIN already
specified.

## Related

- [Track tail interop](/quest/m1/track-tail-interop.md) - proves the close across languages through a relay, which this makes hold for JS publishers
