# [S] A stuck CARLA truck respawns after the stuck timeout, including after a handback

## Goal

A CARLA truck nobody is driving that stays wedged for `STUCK_TIMEOUT` (300 s)
respawns, whether it got there under Traffic Manager or after a viewer handed
it back. Observed on the public demo: after a manual-control handback a truck
sat at 0 km/h for 8 minutes and never respawned, until a viewer took it again
and pressed Reset. A test reproduces the post-handback case.

## Plan

The detector (`StuckDetector` in `demo/carla/sim/moq_carla_sim/runtime.py`)
runs on the telemetry tick in `TruckSession._publish_telemetry`, and is reset
on every tick where the truck has a driver, is e-stopped, is not
`Truck.autonomous`, or the world is parked. Reproduce the observed sequence
first (take control, drive off the road, E-stop, reset, hand back, wedge),
then find which condition holds it reset. Suspects:

- `not self._truck.autonomous`: after a handback the truck stays
  non-autonomous while `handback.plan` keeps returning a control, and only
  engages autonomy once that returns `None`. A truck the handback keeps
  steering without moving disarms the detector forever.
- The lease or latch outliving the handback: a driver that is still the
  holder, or an e-stop nobody reset.

Recommended fix, if the first suspect holds: arm the detector for any
driverless, un-e-stopped truck in a running world, autonomous or mid-handback;
a respawn already ends the handback. A held truck and a latched e-stop stay
exempt, as the `_publish_telemetry` docstring says. Test it in
`sim/tests/test_runtime.py` beside
`test_autonomous_truck_wedged_for_the_timeout_is_respawned`, with the fake
clock, not wall time.

## Related

- [Taking control of a CARLA truck starts from neutral pedals](/quest/m1/carla-takeover-neutral.md) - the takeover that sent the observed truck off the road
- [CARLA on sockets](/quest/m1/socket/carla.md) - moves the lease and handback; the stuck rule must survive the move
