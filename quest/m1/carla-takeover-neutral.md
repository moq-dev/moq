# [XS] Taking control of a CARLA truck starts from neutral pedals

## Goal

When a viewer takes control of a CARLA truck, the truck gets zero throttle
until the viewer presses a pedal, instead of the autopilot's last pedal
values. Today a truck taken while Traffic Manager was accelerating keeps about
85% throttle until the viewer hits E-stop, and drives off the road. A test of
the control handoff covers it.

## Plan

Takeover is bumpless by design today: `takeoverState` (`demo/carla/web/src/input.ts`)
seeds the driver's state from the truck's measured controls, and the worker's
`Drive.synchronize` (`demo/carla/sim/moq_carla_sim/drive.py`) adopts them before
the first claim. Keep that only for steering and gear, so the wheel does not
jump and the claim lands in the right gear. Pedals start released.

- Brake: recommended, the brake is applied until the viewer's first input, so
  a new driver who does nothing brings the truck to rest instead of coasting it
  off the road. The other option is zero brake (a coast). Pick in the PR and
  record why. Mind `Truck.actuate`'s rule that a resting-state claim leaves
  Traffic Manager in control, and the shift interlock, which wants some brake.
- Both sides: the browser seeds the new state, and the worker clamps a
  claiming command's pedals the same way, so an old or hostile client cannot
  claim at full throttle either.
- Tests: rewrite `test_takeover_synchronizes_gate_with_current_vehicle_controls`
  (`sim/tests/test_runtime.py`) and the `beginTakeover` test in
  `web/src/truck.test.ts` to assert the new pedals while steering and gear
  still follow the truck. Fix the `world.py` module docstring, which says
  takeover keeps the pedal demand.

## Related

- [CARLA on sockets](/quest/m1/socket/carla.md) - moves the control track; the handoff behaves the same after the move
- [A stuck CARLA truck respawns after a handback](/quest/m1/carla-stuck-handback.md) - the other half of a bad takeover: getting the truck back
