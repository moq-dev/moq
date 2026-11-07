# [S] CARLA's driver ids stay private

## Goal

A CARLA driver's endpoint id is 128 random bits, like a boy viewer's, and the
truck's public `status` track shows a handle that isn't the endpoint id. So no
visitor learns a lease holder's id from `status`, and none can announce it to
disrupt that driver's next session.

## Plan

Decided 2026-11-06 (maintainer, `/quest-complete` on
[moq-dev/moq#8011](https://github.com/moq-dev/moq/pull/8011)): let #8011
land and fix this after it. Not chosen: fix it in #8011 first; accept it as a
cooperative demo.

After #8011, a driver id is `driver-<12 hex>` (48 bits), the lease binds to
`(driver_id, session)`, and `status` echoes `"driver": "driver-ab12cd"`.
Every viewer reads `status`, and two announcements of one path coexist with
the relay picking a route, so an impostor that announces a leaked id can win
the route to it. It can't drive (the session id stays secret), but it can break
the holder's next dial.

- Generate 128-bit driver ids in the web driver, as `js/moq-boy` does.
- Publish a display handle in `status`, such as a short hash of the session or
  a per-lease counter, that the truck maps to its lease. `driver` and
  `estop_by` keep showing who holds the truck.
- If [socket roles](/quest/m1/socket-roles.md) lands first, the ids live
  under `.client/` and need no `driver-` prefix.

Public API: none. Wire: none.
