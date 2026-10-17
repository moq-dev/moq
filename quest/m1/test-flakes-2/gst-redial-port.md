# [XS] moq-gst redial tests keep their relay's port

## Goal

The moq-gst redial tests (`a_refused_redial_posts_a_bus_error_after_playback`,
`relay_loss_reconnects_after_the_default_budget`) never fail with
`AddrInUse` under load, fixed at its cause.

## Plan

Seen at least three times on #5798, #5803, #5815 and #5820 runs. The tests
stop their test relay (`Relay::bind`/`stop` in
`rs/moq-gst/src/source/imp.rs`) and rebind the freed ephemeral port to bring
it back; on a loaded host a parallel test can take that port in between.
The cause is unconfirmed: reproduce it first. Never add a retry around the
bind.

Public API: none. Wire: none.
