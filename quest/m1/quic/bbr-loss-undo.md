# [S] Preserve BBR state across a spurious loss episode

## Goal

Declaring a loss episode spurious restores the state saved before that
episode, even when several packets were declared lost. Later losses in the
same episode cannot overwrite the original recovery snapshot.

## Plan

[note_loss](https://github.com/n0-computer/noq/blob/1a26a8b064d21e316fe6769f068617975bd8a27b/noq-proto/src/congestion/bbr3/mod.rs#L1513) saves undo state on every lost packet, including
after an earlier packet reduced the model. With a 100,000-byte long-term
inflight bound, a 10,000-byte BDP, a 20,000-byte cwnd, and two successive
1200-byte packet losses during ProbeUp, undo retained 7000 bytes rather than
the original bound. Existing single-loss simulations miss this case.

Align snapshot lifetime with recovery semantics using
[Google Linux's recovery entry](https://github.com/google/bbr/blob/90210de4b779d40496dee0b89081780eeddf2a60/net/ipv4/tcp_bbr.c#L2240) and
[draft section 5.5.11](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-5.5.11). Verify saved model bounds,
window, and any restorable phase across multiple losses in one event and
across ACK events. A new episode must get a new snapshot; real loss must
still constrain sending. Cover ProbeRTT interaction and add regressions to
the fork's CI. Keep the fix internal with no wire change.

## Related

- [Release BBR fixes](/quest/m1/quic/bbr-release.md) - deliver the corrected controller to MoQ
- [Upstream the fork](/quest/m1/quic/upstream.md) - offer general fixes upstream
- [BBR3 app-limited](/quest/m2/quic-bbr-app-limited.md) - measure the corrected controller on media traffic
