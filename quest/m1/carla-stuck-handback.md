# [S] A wedged CARLA truck nobody drives respawns after the stuck timeout

## Goal

A CARLA truck nobody is driving that stays wedged for `STUCK_TIMEOUT` (300 s)
respawns, including after a viewer hands it back. Observed on the public
demo: after a manual-control handback a truck sat at 0 km/h for 8 minutes and
never respawned, until a viewer took it again and pressed Reset. The PR names
the condition that kept the detector disarmed in that case, and a test
reproduces it.

## Plan

The fake-time reproduction in
`demo/carla/sim/tests/test_runtime.py::test_stuck_watchdog_after_manual_estop_and_handback`
uses the real command loop, `Drive`, and `Truck.actuate`/`Truck.release`
with stationary vehicle feedback. Manual pedal input, e-stop, and either an
explicit handback or command-watchdog expiry leave no driver but keep the
e-stop latched and autonomy off. With the world running and no displacement,
the detector stays reset for all 480 s. Resetting e-stop before the same
handback engages autonomy and respawns the wedged truck after 300 s.

The latch is therefore sufficient to reproduce the reported symptom:
`TruckSession._publish_telemetry` excludes `self._drive.estopped`, skips
`Truck.release`, and also excludes the resulting `not self._truck.autonomous`.
Removing just the e-stop exclusion would still leave the detector disarmed.
The supplied report does not include latch/demand/pose history, so this does
not establish whether its viewer reset e-stop before handing back.

Ruled out by the code: a stalled handback. The same tick calls
`Truck.release` for every driverless, un-e-stopped truck, and `handback.plan`
returns `None` at or below `WALKING_PACE` (1 m/s), so a truck at 0 km/h
engages autonomy within one tick. An abandoned lease also cannot hold: the
command watchdog drops it after `COMMAND_TIMEOUT` (1 s).

Once the maintainer selects the policy, implement it at the telemetry
watchdog and update the reproducer's expectations. If automatic recovery is
selected, a driverless latched truck must be eligible despite autonomy being
off, and respawn must clear the gate's latch and attribution as well as move
the actor. Keep held trucks exempt and parked worlds disarmed. Preserve the
takeover pedal hold from [moq-dev/moq#7798](https://github.com/moq-dev/moq/pull/7798).

If the incident's e-stop had been reset, investigate the remaining conditions
with demand and pose history: a parked world resets the detector, and moving
`STUCK_DISTANCE` re-anchors it. Neither is needed for the latch reproduction.

## Waiting

- Maintainer: choose whether a stationary driverless truck with a latched e-stop
  and a running world respawns after 300 s and clears the latch (recommended
  for the public demo, since nobody may return to reset it), or stays stopped
  until an explicit reset (and narrow the Goal accordingly). This changes the
  deliberate e-stop policy; tests can demonstrate both outcomes but cannot
  decide it. Clear this wait when a maintainer records the choice on the PR.

## Related

- [Taking control of a CARLA truck starts from neutral pedals](/quest/m1/carla-takeover-neutral.md) - the takeover that sent the observed truck off the road
- [CARLA on sockets](/quest/m1/socket/carla.md) - moves the lease and handback; the stuck rule must survive the move
