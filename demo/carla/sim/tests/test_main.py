import asyncio
import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from moq_carla_sim import main as main_module
from moq_carla_sim.main import broadcast_path, default_trucks, driver_prefix, parse_args
from moq_carla_sim.watchdog import Heartbeat


def test_default_trucks_are_the_checked_in_list() -> None:
    expected = json.loads(main_module.TRUCKS_FILE.read_text())
    assert expected
    assert parse_args(["--url", "http://localhost:4443"]).trucks == expected


def test_default_trucks_refuses_a_malformed_list(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    for body in ("[]", '["truck1", ""]', '{"trucks": ["truck1"]}'):
        path = tmp_path / "trucks.json"
        path.write_text(body)
        monkeypatch.setattr(main_module, "TRUCKS_FILE", path)
        with pytest.raises(ValueError, match="non-empty list of truck names"):
            default_trucks()


def test_relay_url_defaults_from_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("MOQ_URL", "https://relay.example.test/demo?jwt=secret")
    assert parse_args([]).url == "https://relay.example.test/demo?jwt=secret"


def test_relay_url_defaults_to_the_local_relay(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("MOQ_URL", raising=False)
    assert parse_args([]).url == "http://localhost:4443"


def test_paths_match_the_player() -> None:
    assert broadcast_path("truck1", "front") == "carla/truck1/front.hang"
    assert driver_prefix("truck1") == "viewer/carla/truck1"


def test_log_level_configures_python_and_rust(monkeypatch: pytest.MonkeyPatch) -> None:
    levels = []

    async def fake_run(args: SimpleNamespace) -> None:
        levels.append(args.log_level)

    monkeypatch.setattr(main_module.moq, "log_level", levels.append, raising=False)
    monkeypatch.setattr(main_module, "run", fake_run)

    assert main_module.main(["--url", "http://localhost:4443", "--log-level", "debug"]) == 0
    assert levels == ["debug", "debug"]


def test_bridge_socket_defaults_to_where_the_server_puts_it(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("CARLA_BRIDGE_SOCKET", raising=False)
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/1000")
    assert parse_args(["--url", "x"]).bridge_socket == "/run/user/1000/carla-bridge/bridge.sock"
    monkeypatch.setenv("CARLA_BRIDGE_SOCKET", "/elsewhere.sock")
    assert parse_args(["--url", "x"]).bridge_socket == "/elsewhere.sock"


def test_a_missing_bridge_says_how_the_simulator_must_run(bridge_socket) -> None:
    with pytest.raises(RuntimeError, match="-moq-bridge-socket"):
        asyncio.run(main_module.connect_bridge(bridge_socket))


async def never_fails() -> None:
    await asyncio.Event().wait()


def supervise_with(closed, clock_failed=never_fails) -> tuple[BaseException | None, asyncio.Task]:
    """Run `supervise` against a truck that never ends and a session that does.

    Returns whatever ended the run plus the truck task, so a test can assert it
    was cancelled and awaited rather than left to loop teardown.
    """

    async def scenario() -> tuple[BaseException | None, asyncio.Task]:
        client = SimpleNamespace(session=SimpleNamespace(closed=closed))
        clock = SimpleNamespace(wait_failed=clock_failed)
        watchdog = SimpleNamespace(pulse=never_fails)
        truck = asyncio.create_task(asyncio.Event().wait(), name="truck:truck1")
        try:
            await asyncio.wait_for(main_module.supervise(client, [truck], clock, watchdog), timeout=1.0)
        except BaseException as err:  # the test asserts on it
            return err, truck
        return None, truck

    return asyncio.run(scenario())


def test_dead_session_ends_the_run() -> None:
    """A session that stops for good must fail the process, not be ignored.

    The trucks keep running against a dead session: their tracks accept frames
    and no task ever finishes, so without this the relay announces nothing while
    systemd sees a healthy worker.
    """

    async def closed() -> None:
        return None

    err, truck = supervise_with(closed)
    assert isinstance(err, RuntimeError)
    assert "relay session closed" in str(err)
    assert truck.cancelled()


def test_dead_session_surfaces_the_transport_error() -> None:
    async def closed() -> None:
        raise main_module.moq.Error("UnexpectedEnd")

    err, _ = supervise_with(closed)
    assert isinstance(err, main_module.moq.Error)
    assert "UnexpectedEnd" in str(err)


def test_stopped_clock_ends_the_run() -> None:
    """A world nobody ticks is frozen, and nothing else would notice."""

    async def closed() -> None:
        await asyncio.Event().wait()

    async def clock_failed() -> None:
        raise RuntimeError("simulation clock stopped")

    err, truck = supervise_with(closed, clock_failed)
    assert isinstance(err, RuntimeError)
    assert "simulation clock stopped" in str(err)
    assert truck.cancelled()


def test_second_worker_on_the_same_simulator_is_refused(monkeypatch: pytest.MonkeyPatch, tmp_path) -> None:
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))

    first = main_module.claim_simulator("127.0.0.1", 2000)
    with pytest.raises(RuntimeError, match="another worker already owns the simulator at 127.0.0.1:2000"):
        main_module.claim_simulator("127.0.0.1", 2000)
    # A different simulator is a different world.
    main_module.claim_simulator("127.0.0.1", 2001).close()

    first.close()
    main_module.claim_simulator("127.0.0.1", 2000).close()


def test_sigterm_unwinds_like_ctrl_c(monkeypatch: pytest.MonkeyPatch) -> None:
    """Before the loop's handlers exist, SIGTERM must reach `run`'s cleanup."""
    handlers = {}
    monkeypatch.setattr(main_module.signal, "signal", lambda signum, handler: handlers.__setitem__(signum, handler))
    monkeypatch.setattr(main_module.moq, "log_level", lambda _level: None, raising=False)

    async def fake_run(_args) -> None:
        handlers[main_module.signal.SIGTERM](main_module.signal.SIGTERM, None)

    monkeypatch.setattr(main_module, "run", fake_run)

    assert main_module.main(["--url", "http://localhost:4443"]) == 0


def test_actors_are_destroyed_before_the_relay_session_closes(monkeypatch: pytest.MonkeyPatch, tmp_path) -> None:
    """The actors outlive the process; a relay close that hangs must not stand between them and cleanup."""
    events: list[str] = []
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))

    class FakeWorld:
        trucks: list = []

        def __init__(self) -> None:
            self.clock = SimpleNamespace(start=lambda: events.append("clock.start"), heartbeat=Heartbeat("clock"))

        def sweep(self) -> None:
            events.append("sweep")

        def spawn(self, _names, _camera, _camera_name) -> None:
            events.append("spawn")

        def map_name(self) -> str:
            return "Town"

        def ready(self) -> None:
            events.append("ready")

        def destroy(self) -> None:
            events.append("destroy")

    class FakeConnect:
        async def __aenter__(self):
            events.append("session.open")
            return SimpleNamespace()

        async def __aexit__(self, *_exc) -> None:
            events.append("session.close")

    async def fake_supervise(_client, _tasks, _clock, _watchdog) -> None:
        events.append("supervise")

    class FakeBridge:
        async def run(self) -> None:
            await asyncio.Event().wait()

        def disconnect(self) -> None:
            events.append("bridge.disconnect")

    async def fake_connect_bridge(_path: str) -> FakeBridge:
        events.append("bridge.connect")
        return FakeBridge()

    monkeypatch.setattr(main_module.World, "connect", staticmethod(lambda *_args: FakeWorld()))
    monkeypatch.setattr(main_module, "connect_bridge", fake_connect_bridge)
    monkeypatch.setattr(main_module.moq, "connect", lambda _url: FakeConnect(), raising=False)
    monkeypatch.setattr(main_module, "supervise", fake_supervise)

    asyncio.run(main_module.run(parse_args(["--url", "http://localhost:4443"])))

    assert events == [
        "clock.start",
        "sweep",
        "spawn",
        # The clock is only allowed to park once every truck is in the world:
        # `sweep` and `spawn` both wait on ticks.
        "ready",
        # The cameras are in the world, so the simulator's bridge is listening.
        "bridge.connect",
        "session.open",
        "supervise",
        "destroy",
        "session.close",
        "bridge.disconnect",
        "destroy",
    ]
