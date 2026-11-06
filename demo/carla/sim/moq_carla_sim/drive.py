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

    `driver_id` is the driver's socket endpoint id, which `status` echoes.
    `session` is the socket session that carried it, one per lease the player
    takes: the same id on another session is a different driver for the lease.
    The session id is the secret that keeps a session to its two ends, so it
    never leaves this process.
    """

    kind: CommandKind
    driver_id: str = ""
    session: str | None = None
    sequence: int = 0
    state: DriveState | None = None
    timestamps: tuple[TimestampEntry, ...] = ()


class Drive:
    """The truck's live control state machine.

    The first driver to send a command owns the truck until it disconnects or
    its command lease expires. Other drivers cannot silently steal control.

    The lease binds to the driver's socket session, so a driver whose session
    ends loses it, and another session claiming the same driver id is another
    driver. It is still cooperative, not enforced: anonymous drivers share one
    grant, so anyone can announce an endpoint under the holder's id, which the
    public `status` track echoes. Their own sessions never continue the
    holder's lease, but they can disrupt it: a relay may route the truck's
    accept of the holder's next session to the impostor instead. This stops two
    honest drivers colliding; it does not stop a deliberate disruption.
    """

    def __init__(self) -> None:
        self._driver: str | None = None
        self._session: str | None = None
        self._state = DriveState.stopped()
        self._takeover_pedals: tuple[float, float] | None = None
        self._estopped = False
        self._estop_by: str | None = None

    @property
    def driver(self) -> str | None:
        return self._driver

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
        """Whether `command` comes from the session holding the lease."""
        return self._driver is not None and (self._driver, self._session) == (command.driver_id, command.session)

    def accepted(self) -> Accepted:
        return Accepted(state=self._state, estopped=self._estopped)

    def apply(self, command: Command) -> bool:
        """Apply a command, returning whether it changed or refreshed state."""
        if command.kind == "drive":
            if self._estopped or not (self._driver is None or self.holds(command)):
                return False
            claiming = self._driver is None
            self._driver = command.driver_id
            self._session = command.session
            state = (command.state or DriveState.stopped()).sanitized()
            pedals = (state.throttle, state.brake)
            if claiming:
                self._takeover_pedals = pedals
            if self._takeover_pedals == pedals:
                # Heartbeats can repeat the autopilot's demand indefinitely.
                # Only a pedal change releases the takeover brake.
                state = replace(state, throttle=0.0, brake=1.0)
            else:
                self._takeover_pedals = None
            if state.gear is not self._state.gear and not self._can_shift(state):
                state = replace(state, gear=self._state.gear)
            self._state = state

        elif command.kind == "estop":
            was_estopped = self._estopped
            # E-stop is a safety control anyone may trigger, so it always
            # latches and brings the truck to a stop, but it must never transfer
            # the driving lease to its sender. Otherwise a bystander could
            # e-stop, then reset_estop, and drive off with a lease they never
            # legitimately claimed.
            self._state = DriveState.stopped()
            self._estopped = True
            if not was_estopped:
                self._estop_by = command.driver_id

        elif command.kind == "reset_estop":
            if not self._estopped:
                return False
            # Reset clears the global latch without claiming or transferring the
            # lease.
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

    def _clear_driver(self) -> None:
        self._driver = None
        self._session = None
        self._state = DriveState.stopped()
        self._takeover_pedals = None
