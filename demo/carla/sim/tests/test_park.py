from __future__ import annotations

from types import SimpleNamespace

import pytest

from moq_carla_sim import park as park_module


def fake_world(no_rendering: bool) -> SimpleNamespace:
    settings = SimpleNamespace(no_rendering_mode=no_rendering)
    world = SimpleNamespace(
        settings=settings,
        applied=[],
        get_settings=lambda: settings,
    )
    world.apply_settings = lambda value: world.applied.append(value.no_rendering_mode)
    return world


def patch_client(monkeypatch: pytest.MonkeyPatch, world: SimpleNamespace) -> None:
    monkeypatch.setattr(park_module, "connect_client", lambda *_args: SimpleNamespace(get_world=lambda: world))


def test_park_disables_rendering_on_an_unowned_simulator(monkeypatch: pytest.MonkeyPatch, tmp_path) -> None:
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    world = fake_world(no_rendering=False)
    patch_client(monkeypatch, world)

    assert park_module.park("127.0.0.1", 2000, 30.0)
    assert world.applied == [True]


def test_park_leaves_a_worker_owned_simulator_alone(monkeypatch: pytest.MonkeyPatch, tmp_path) -> None:
    """Parking under a live worker would black out a viewer mid-stream."""
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    world = fake_world(no_rendering=False)
    patch_client(monkeypatch, world)

    from moq_carla_sim.main import claim_simulator

    worker = claim_simulator("127.0.0.1", 2000)
    try:
        assert not park_module.park("127.0.0.1", 2000, 30.0)
        assert world.applied == []
    finally:
        worker.close()


def test_park_releases_the_lock_so_the_worker_can_start(monkeypatch: pytest.MonkeyPatch, tmp_path) -> None:
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    patch_client(monkeypatch, fake_world(no_rendering=False))

    from moq_carla_sim.main import claim_simulator

    assert park_module.park("127.0.0.1", 2000, 30.0)
    claim_simulator("127.0.0.1", 2000).close()


def test_park_is_idempotent(monkeypatch: pytest.MonkeyPatch, tmp_path) -> None:
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    world = fake_world(no_rendering=True)
    patch_client(monkeypatch, world)

    assert park_module.park("127.0.0.1", 2000, 30.0)
    assert world.applied == []


def test_park_never_touches_synchronous_mode(monkeypatch: pytest.MonkeyPatch, tmp_path) -> None:
    """A synchronous world nobody ticks is frozen for carla-map and the benchmark."""
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    world = fake_world(no_rendering=False)
    world.settings.synchronous_mode = False
    world.settings.fixed_delta_seconds = 0.0
    patch_client(monkeypatch, world)

    park_module.park("127.0.0.1", 2000, 30.0)

    assert world.settings.synchronous_mode is False
    assert world.settings.fixed_delta_seconds == 0.0
