# [M] Operator arbitration

## Goal

Exactly one controller commands a vehicle at a time, with explicit handoff, and
the boundary between arbitration and authorization is stated rather than
assumed.

## Plan

`moq-boy` merges every viewer's input, which is right for crowd control and
wrong for a vehicle. Arbitration is the part of a teleoperation stack that
cannot be lifted from it: one controller is active, the others are read-only,
and handoff is explicit rather than last-writer-wins.

### Arbitration is not authorization

The robot subscribes an announce prefix that is a sibling of what it publishes,
so on its own nothing stops anyone able to announce under `control/<id>/` from
joining the pool. `moq-boy`'s fan-in is safe because a hostile viewer can only
press B; a vehicle is not that.

Admission belongs to the relay as a path-scoped grant, not to this crate.
Scoped signing keys landed in moq#2416 (`6b86e612b`), so the enforcing
mechanism already exists: `Scope` in `rs/moq-token/src/claims.rs` and
`Key::with_scope` in `key.rs` bind a key to publish and subscribe path
prefixes. Write the concrete claims down (a robot publishes `robot/<id>` and
subscribes `control/<id>/`; an operator publishes `control/<id>/<operator>` and
subscribes `robot/<id>`) and prove with a relay test that a key scoped
elsewhere cannot announce under `control/<id>/`. Documenting the requirement is
not an acceptable outcome now that enforcement ships. Revoking a superseded or
compromised operator's grant mid-session is relay auth work, not something
this crate implements; scoped keys only gate admission.

Link loss is the other half: a class whose contract is "drop stale samples"
needs a stated behavior when samples stop arriving, and the vehicle's own
failsafe is what actually holds. Say what the crate reports and what it does
not attempt.

## Required

- [Robot teleoperation primitive](/quest/m3/teleop/robot.md)
