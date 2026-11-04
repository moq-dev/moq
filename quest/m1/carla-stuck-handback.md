# [S] A wedged CARLA truck nobody drives respawns after the stuck timeout

## Goal

A CARLA truck nobody is driving that stays wedged for `STUCK_TIMEOUT` (300 s)
respawns, including after a viewer hands it back. Observed on the public
demo: after a manual-control handback a truck sat at 0 km/h for 8 minutes and
never respawned, until a viewer took it again and pressed Reset. The PR names
the condition that kept the detector disarmed in that case, and a test
reproduces it.

## Plan

The detector (`StuckDetector` in `demo/carla/sim/moq_carla_sim/runtime.py`)
runs on the telemetry tick in `TruckSession._publish_telemetry`, and is reset
on every tick where the truck has a driver, is e-stopped, is not
`Truck.autonomous`, or the world is parked. Reproduce the observed sequence
first (take control, drive off the road, E-stop, hand back, wedge), noting
whether the e-stop was reset before the handback, then find which condition
held it reset.

Ruled out by the code: a stalled handback. The same tick calls
`Truck.release` for every driverless, un-e-stopped truck, and `handback.plan`
returns `None` at or below `WALKING_PACE` (1 m/s), so a truck at 0 km/h
engages autonomy within one tick. An abandoned lease also cannot hold: the
command watchdog drops it after `COMMAND_TIMEOUT` (1 s).

Suspects, most likely first:

- A latched e-stop. An e-stop set before the handback and never reset exempts
  the truck forever, on purpose (`_publish_telemetry`'s docstring). Whether a
  driverless, latched truck should still respawn after the timeout, and clear
  the latch when it does, is a maintainer decision for the PR. Recommended:
  yes, since on a public demo nobody may come back to reset it.
- A parked world (`not self._clock.running`): check whether status or video
  demand held the clock for the full 8 minutes.
- A truck that rocks against what wedges it: one that moves `STUCK_DISTANCE`
  re-anchors the detector and restarts the clock.

Test the cause in `sim/tests/test_runtime.py` beside
`test_autonomous_truck_wedged_for_the_timeout_is_respawned`, with the fake
clock, not wall time.

## Related

- [CARLA on sockets](/quest/m1/socket/carla.md) - moves the lease and handback; the stuck rule must survive the move
