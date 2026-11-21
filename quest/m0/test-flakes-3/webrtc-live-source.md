# [S] The WHIP and WHEP round trips play under load

## Goal

The `moq-webrtc` tests that wait on a packet the source pushed once before its viewer connected pass however loaded the host is: `a_whep_viewer_ends_with_its_broadcast`, `a_whep_viewer_joining_after_the_catalog_ends_plays_the_rest`, `a_whep_viewer_stays_on_its_instance_until_the_route_dies`, `a_whip_push_stays_on_its_instance_until_the_route_dies`, and `whip_and_whep_round_trip_opus`.

## Plan

Found while fixing [a_reoffer_from_the_same_address_plays](https://github.com/moq-dev/moq/pull/9241), which failed for the same cause. Run sixteen copies of one of these at once inside `systemd-run --user --scope -p CPUQuota=100%` and every copy times out at 10 s waiting for its first packet. None has flaked in a real `just check` yet, only a_reoffer did.

The cause: `Bridge::push` cuts a group per packet, so a packet's group has an (empty) successor at once and goes stale 100 ms later (`egress::MAX_DELAY`). A session that connects past that budget under load reads nothing, and the test's source never pushes again. `a_reoffer_from_the_same_address_plays` now keeps a live source sending (`played_live`) until its viewer plays. Do the same here, sharing the helper rather than copying it: the sources that also need an exact payload (`OLD_PACKET` after a take-over) push it as their live packet instead of a one-shot.
