"""The control safety gate's contract."""

from dataclasses import replace

import pytest

from moq_carla_sim.drive import Command, Drive, DriveState, Gear


def drive(driver_id: str, sequence: int, throttle: float) -> Command:
    return Command(
        kind="drive",
        driver_id=driver_id,
        sequence=sequence,
        state=DriveState(throttle=throttle, brake=0.0),
    )


def estop(driver_id: str, sequence: int) -> Command:
    return Command(kind="estop", driver_id=driver_id, sequence=sequence)


def reset(driver_id: str, sequence: int) -> Command:
    return Command(kind="reset_estop", driver_id=driver_id, sequence=sequence)


def test_first_driver_keeps_control() -> None:
    state = Drive()
    assert state.apply(drive("first", 1, 0.5))
    assert not state.apply(drive("second", 2, 1.0))
    assert state.driver == "first"
    assert state.state.throttle == 0.0
    assert state.state.brake == 1.0
    assert state.apply(drive("first", 2, 0.6))
    assert state.state.throttle == 0.6


def test_the_lease_belongs_to_one_run_of_a_driver() -> None:
    """A driver id announced under another instance is another driver: it cannot
    steer while the first run holds the lease, nor release it."""
    state = Drive()
    first = Command(
        kind="drive", driver_id="abc", instance="run-1", sequence=1, state=DriveState(throttle=0.5, brake=0.0)
    )
    second = Command(
        kind="drive", driver_id="abc", instance="run-2", sequence=1, state=DriveState(throttle=1.0, brake=0.0)
    )
    assert state.apply(Command(kind="drive", driver_id="abc", instance="run-1"))
    assert state.apply(first)
    assert not state.apply(second)
    assert state.state.throttle == 0.5
    assert not state.apply(Command(kind="driver_left", driver_id="abc", instance="run-2"))
    assert state.driver == "abc"

    # The first run leaving frees the lease for the second.
    assert state.apply(Command(kind="driver_left", driver_id="abc", instance="run-1"))
    assert state.driver is None
    assert state.apply(second)
    assert state.holds(second)
    assert state.driver == "abc"


def test_estop_latches_until_explicit_reset() -> None:
    state = Drive()
    assert state.apply(drive("driver", 1, 1.0))
    assert state.apply(estop("driver", 2))
    assert state.estopped
    assert not state.apply(drive("driver", 3, 1.0))
    assert state.state == DriveState.stopped()
    assert state.apply(reset("driver", 4))
    assert not state.estopped
    assert state.state == DriveState.stopped()


def test_watchdog_stops_and_releases_driver() -> None:
    state = Drive()
    assert state.apply(drive("driver", 1, 1.0))
    assert state.apply(Command(kind="watchdog_expired"))
    assert state.driver is None
    assert state.sequence is None
    assert state.state == DriveState.stopped()


def test_disconnect_does_not_clear_estop() -> None:
    state = Drive()
    assert state.apply(drive("driver", 1, 1.0))
    assert state.apply(estop("driver", 2))
    assert state.estopped
    assert state.driver == "driver"

    # The driver disconnects: the lease releases but the latch must persist, so
    # the truck stays stopped until an explicit reset.
    assert state.apply(Command(kind="driver_left", driver_id="driver"))
    assert state.estopped
    assert state.driver is None


def test_estop_from_a_bystander_latches_without_transferring_the_lease() -> None:
    state = Drive()
    assert state.apply(drive("first", 1, 0.5))

    # Anyone may hit the big red button, so the latch must engage...
    assert state.apply(estop("second", 2))
    assert state.estopped
    assert state.estop_by == "second"

    # ...but the lease stays with "first", and the bystander's sequence is not
    # recorded as the authoritative command.
    assert state.driver == "first"
    assert state.sequence == 1

    # The holder heartbeat is acknowledged without replacing the actor that
    # originally latched the stop.
    assert state.apply(estop("first", 3))
    assert state.estop_by == "second"
    assert state.sequence == 3

    # Anyone may clear the global latch, but doing so neither transfers the
    # lease nor records the bystander's sequence as authoritative.
    assert state.apply(reset("second", 4))
    assert not state.estopped
    assert state.estop_by is None
    assert state.driver == "first"
    assert state.sequence == 3
    assert not state.apply(drive("second", 5, 1.0))


def test_estop_from_holder_refreshes_the_acknowledgement_sequence() -> None:
    state = Drive()
    assert state.apply(drive("holder", 5, 0.3))
    assert state.sequence == 5

    # The holder's own e-stop is acknowledged (sequence advances) and it keeps
    # the lease.
    assert state.apply(estop("holder", 6))
    assert state.estopped
    assert state.sequence == 6
    assert state.driver == "holder"


def test_axes_are_clamped_into_range() -> None:
    state = Drive()
    assert state.apply(Command(kind="drive", driver_id="driver", sequence=1))
    assert state.apply(
        Command(
            kind="drive",
            driver_id="driver",
            sequence=2,
            state=DriveState(steering=-4.0, throttle=9.0, brake=-1.0, gear=Gear.FORWARD),
        )
    )
    accepted = state.state
    assert accepted.steering == -1.0
    assert accepted.throttle == 1.0
    assert accepted.brake == 0.0
    # Clamping happens before the interlock, so invalid pedal values cannot
    # smuggle a gear change through.
    assert accepted.gear is Gear.NEUTRAL


@pytest.mark.parametrize("gear", list(Gear))
def test_every_new_lease_resets_pedals_without_changing_steering_or_gear(gear: Gear) -> None:
    state = Drive()
    command = Command(
        kind="drive",
        driver_id="driver",
        sequence=1,
        state=DriveState(steering=0.3, throttle=1.0, brake=0.0, gear=gear),
    )
    assert state.apply(command)
    assert state.state == DriveState(steering=0.3, throttle=0.0, brake=1.0, gear=gear)

    # Heartbeats and steering-only input must not restore inherited throttle.
    assert state.apply(command)
    assert state.state == DriveState(steering=0.3, throttle=0.0, brake=1.0, gear=gear)
    assert state.apply(replace(command, state=DriveState(steering=-0.4, throttle=1.0, brake=0.0, gear=gear)))
    assert state.state == DriveState(steering=-0.4, throttle=0.0, brake=1.0, gear=gear)

    pedal_input = DriveState(steering=-0.4, throttle=0.5, brake=0.0, gear=gear)
    assert state.apply(replace(command, state=pedal_input))
    assert state.state == pedal_input
    assert state.apply(command)
    assert state.state == command.state

    assert state.apply(Command(kind="watchdog_expired"))
    assert state.apply(command)
    assert state.state == DriveState(steering=0.3, throttle=0.0, brake=1.0, gear=gear)

    assert state.apply(command)
    assert state.state == DriveState(steering=0.3, throttle=0.0, brake=1.0, gear=gear)


def test_takeover_brake_allows_a_forward_claim_without_autonomous_feedback() -> None:
    state = Drive()
    assert state.apply(
        Command(kind="drive", driver_id="driver", state=DriveState(throttle=0.85, brake=0.0, gear=Gear.FORWARD))
    )
    assert state.state == DriveState(gear=Gear.FORWARD)


def test_gear_change_requires_brake_and_released_throttle() -> None:
    state = Drive()

    # Establish the lease before testing ordinary gear changes.
    assert state.apply(
        Command(
            kind="drive",
            driver_id="driver",
            sequence=1,
            state=DriveState.stopped(),
        )
    )

    assert state.apply(
        Command(
            kind="drive",
            driver_id="driver",
            sequence=2,
            state=DriveState(throttle=0.0, brake=0.0, gear=Gear.FORWARD),
        )
    )
    assert state.state.gear is Gear.NEUTRAL

    assert state.apply(
        Command(
            kind="drive",
            driver_id="driver",
            sequence=3,
            state=DriveState(throttle=0.01, brake=0.001, gear=Gear.FORWARD),
        )
    )
    assert state.state.gear is Gear.FORWARD

    assert state.apply(
        Command(
            kind="drive",
            driver_id="driver",
            sequence=4,
            state=DriveState(throttle=0.02, brake=1.0, gear=Gear.REVERSE),
        )
    )
    assert state.state.gear is Gear.FORWARD


def test_resting_state_is_stopped_with_the_brake_on() -> None:
    # A truck nobody is driving must hold the brake rather than coast.
    resting = Drive()
    assert resting.driver is None
    assert resting.state == DriveState(steering=0.0, throttle=0.0, brake=1.0, gear=Gear.NEUTRAL)
    assert not resting.estopped
