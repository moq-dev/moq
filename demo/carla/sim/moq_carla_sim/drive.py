"""The control safety gate.

Every driver command passes through here: sanitize the axes, check the
single-driver lease, honour the latched e-stop. Nothing downstream may act on a
command this gate did not accept, so CARLA's actuation sits strictly below it,
exactly where a real vehicle-bus backend would.
"""

from __future__ import annotations

from dataclasses import dataclass, field, replace
from enum import Enum
from typing import Literal

SHIFT_MAX_THROTTLE = 0.01


class Gear(str, Enum):
    """Transmission gear, as the `control` and `status` tracks spell it."""

    NEUTRAL = "neutral"
    FORWARD = "forward"
    REVERSE = "reverse"


def _clamp(value: float, low: float, high: float) -> float:
    return max(low, min(high, value))


@dataclass(frozen=True)
class DriveState:
    """The actuation request: what the truck has been asked to do right now.

    The defaults are the safe resting state: stopped, brake on, neutral.
    """

    steering: float = 0.0
    throttle: float = 0.0
    brake: float = 1.0
    gear: Gear = Gear.NEUTRAL

    def sanitized(self) -> DriveState:
        """Clamp analog axes into range.

        A misbehaving or malicious driver must never be able to request
        out-of-range actuation.
        """
        return replace(
            self,
            steering=_clamp(self.steering, -1.0, 1.0),
            throttle=_clamp(self.throttle, 0.0, 1.0),
            brake=_clamp(self.brake, 0.0, 1.0),
        )

    @staticmethod
    def stopped() -> DriveState:
        """The all-stop state used for e-stop and when no driver is connected."""
        return DriveState()


@dataclass(frozen=True)
class Accepted:
    """A command the truck has accepted: sanitized, lease-checked, e-stop aware.

    This is the only thing an actuator may act on.
    """

    state: DriveState = field(default_factory=DriveState.stopped)
    estopped: bool = False


@dataclass(frozen=True)
class TimestampEntry:
    """One media-clock timestamp from the driver, in milliseconds."""

    label: str
    ts: float


CommandKind = Literal[
    "drive",
    "estop",
    "reset_estop",
    "respawn",
    "driver_left",
    "watchdog_expired",
]


@dataclass(frozen=True)
class Command:
    """A parsed driver command with the originating driver attached.

    `drive`, `estop`, `reset_estop`, and `respawn` arrive on the wire;
    `driver_left` and `watchdog_expired` are synthesized locally.

    `driver_id` is the driver's path segment, which `status` echoes. `epoch` is
    the route epoch its broadcast was announced under: the same id announced
    under another epoch is another run of the driver, so it is a different
    driver for the lease.
    """

    kind: CommandKind
    driver_id: str = ""
    epoch: str | None = None
    sequence: int = 0
    state: DriveState | None = None
    timestamps: tuple[TimestampEntry, ...] = ()


class Drive:
    """The truck's live control state machine.

    The first driver to send a command owns the truck until it disconnects or
    its command lease expires. Other drivers cannot silently steal control.

    The lease binds to the driver's broadcast path and the epoch it was
    announced under, so a driver whose broadcast ends or restarts loses it.
    It is still cooperative, not enforced: the holder's id is echoed on the
    public `status` track, so anyone allowed to publish there can announce at
    that path under a newer epoch, which ends the holder's broadcast and frees
    the lease for the newcomer. This stops two honest drivers colliding; it
    does not stop a deliberate takeover.
    """

    def __init__(self) -> None:
        self._driver: str | None = None
        self._epoch: str | None = None
        self._sequence: int | None = None
        self._state = DriveState.stopped()
        self._estopped = False
        self._estop_by: str | None = None

    @property
    def driver(self) -> str | None:
        return self._driver

    @property
    def sequence(self) -> int | None:
        return self._sequence

    @property
    def state(self) -> DriveState:
        return self._state

    @property
    def estopped(self) -> bool:
        return self._estopped

    @property
    def estop_by(self) -> str | None:
        return self._estop_by

    def holds(self, command: Command) -> bool:
        """Whether `command` comes from the run of the driver holding the lease."""
        return self._driver is not None and (self._driver, self._epoch) == (command.driver_id, command.epoch)

    def accepted(self) -> Accepted:
        return Accepted(state=self._state, estopped=self._estopped)

    def synchronize(self, state: DriveState) -> bool:
        """Adopt live vehicle feedback before an unclaimed manual handoff."""
        if self._driver is not None or self._estopped:
            return False
        self._state = state.sanitized()
        return True

    def apply(self, command: Command) -> bool:
        """Apply a command, returning whether it changed or refreshed state."""
        if command.kind == "drive":
            if self._estopped or not (self._driver is None or self.holds(command)):
                return False
            claiming = self._driver is None
            self._driver = command.driver_id
            self._epoch = command.epoch
            self._sequence = command.sequence
            state = (command.state or DriveState.stopped()).sanitized()
            if (
                state.gear is not self._state.gear
                and not self._can_shift(state)
                and not (claiming and self._can_initialize_takeover(self._state, state))
            ):
                state = replace(state, gear=self._state.gear)
            self._state = state

        elif command.kind == "estop":
            was_estopped = self._estopped
            # E-stop is a safety control anyone may trigger, so it always
            # latches and brings the truck to a stop, but it must never transfer
            # the driving lease to its sender. Otherwise a bystander could
            # e-stop, then reset_estop, and drive off with a lease they never
            # legitimately claimed. Only refresh the acknowledgement sequence
            # when the sender already holds the lease, so a bystander's e-stop
            # cannot masquerade as the authoritative command either.
            if self.holds(command):
                self._sequence = command.sequence
            self._state = DriveState.stopped()
            self._estopped = True
            if not was_estopped:
                self._estop_by = command.driver_id

        elif command.kind == "reset_estop":
            if not self._estopped:
                return False
            # Reset clears the global latch without claiming or transferring the
            # lease. Only the holder's sequence is authoritative for that lease.
            if self.holds(command):
                self._sequence = command.sequence
            self._state = DriveState.stopped()
            self._estopped = False
            self._estop_by = None

        elif command.kind == "driver_left":
            if not self.holds(command):
                return False
            self._clear_driver()

        elif command.kind == "watchdog_expired":
            if self._driver is None:
                return False
            self._clear_driver()

        return True

    @staticmethod
    def _can_shift(state: DriveState) -> bool:
        """Require some brake and a released throttle for a gear change."""
        return state.brake > 0.0 and state.throttle <= SHIFT_MAX_THROTTLE

    @staticmethod
    def _can_initialize_takeover(current: DriveState, requested: DriveState) -> bool:
        """Let a new CARLA lease synchronize into forward without touching either pedal."""
        return (
            current.gear is Gear.NEUTRAL
            and requested.gear is Gear.FORWARD
            and requested.steering == 0.0
            and requested.throttle == 0.0
            and requested.brake == 0.0
        )

    def _clear_driver(self) -> None:
        self._driver = None
        self._epoch = None
        self._sequence = None
        self._state = DriveState.stopped()
