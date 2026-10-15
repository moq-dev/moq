from __future__ import annotations

import math
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

from moq_carla_sim.aerial import calibration, capture


def test_footprint_contains_roads_and_preserves_world_axes() -> None:
    roads = [-114.2, -68.7, 110.0, 141.1]
    camera, bbox = calibration(roads)
    assert camera.x == pytest.approx(-2.1)
    assert camera.y == pytest.approx(36.2)
    assert camera.pitch == -90
    assert camera.yaw == -90
    assert bbox == pytest.approx([-144.2, -105.9, 140.0, 178.3])
    assert bbox[0] < roads[0] < roads[2] < bbox[2]
    assert bbox[1] < roads[1] < roads[3] < bbox[3]

    # Independent ray projection: a down-facing camera's right/up vectors are
    # (1, 0, 0)/(0, -1, 0) at yaw -90, keeping the road map's x/y orientation.
    half_width = camera.z * math.tan(math.radians(camera.fov / 2))
    for x, y, expected in [(bbox[0], bbox[1], (0, 0)), (bbox[2], bbox[3], (1, 1))]:
        pixel = ((x - camera.x) / (2 * half_width) + 0.5, (y - camera.y) / (2 * half_width) + 0.5)
        assert pixel == pytest.approx(expected)


def test_ground_height_changes_altitude_without_changing_footprint() -> None:
    baseline, footprint = calibration([0, 0, 100, 200], padding=10, fov=40)
    elevated, elevated_footprint = calibration([0, 0, 100, 200], padding=10, ground_z=70, fov=40)
    assert elevated.z == pytest.approx(baseline.z + 70)
    assert elevated_footprint == footprint
    assert (elevated.z - 70) * math.tan(math.radians(20)) == pytest.approx(110)


@pytest.mark.parametrize("bbox", [[], [0, 0, 0, 1], [0, 1, 1, 0], [0, 0, math.nan, 2]])
def test_rejects_invalid_bounds(bbox: list[float]) -> None:
    with pytest.raises(ValueError):
        calibration(bbox)


@pytest.mark.parametrize("options", [{"padding": -1}, {"fov": 0}, {"fov": math.nan}, {"ground_z": math.inf}])
def test_rejects_invalid_camera_options(options: dict[str, float]) -> None:
    with pytest.raises(ValueError):
        calibration([0, 0, 1, 1], **options)


def fake_world(monkeypatch: pytest.MonkeyPatch) -> tuple[Mock, Mock]:
    import carla

    monkeypatch.setattr(carla, "Rotation", lambda **values: SimpleNamespace(**values), raising=False)
    sensor = Mock(id=123)
    sensor.destroy.return_value = True
    world = Mock()
    world.get_settings.return_value = SimpleNamespace(no_rendering_mode=False)
    world.spawn_actor.return_value = sensor
    return world, sensor


def test_capture_timeout_removes_only_its_sensor(monkeypatch: pytest.MonkeyPatch) -> None:
    world, sensor = fake_world(monkeypatch)
    camera, _ = calibration([0, 0, 10, 10])
    with pytest.raises(TimeoutError):
        capture(world, camera, 256, 0.001)
    sensor.stop.assert_called_once()
    sensor.destroy.assert_called_once()
    world.tick.assert_not_called()
    world.apply_settings.assert_not_called()
    world.get_actors.assert_not_called()


def test_listener_failure_still_destroys_sensor(monkeypatch: pytest.MonkeyPatch) -> None:
    world, sensor = fake_world(monkeypatch)
    sensor.listen.side_effect = RuntimeError("stream unavailable")
    sensor.stop.side_effect = RuntimeError("stop unavailable")
    camera, _ = calibration([0, 0, 10, 10])
    with pytest.raises(RuntimeError, match="stop unavailable"):
        capture(world, camera, 256, 1)
    sensor.destroy.assert_called_once()


def test_parked_renderer_does_not_spawn_or_change_settings(monkeypatch: pytest.MonkeyPatch) -> None:
    world, _ = fake_world(monkeypatch)
    world.get_settings.return_value.no_rendering_mode = True
    camera, _ = calibration([0, 0, 10, 10])
    with pytest.raises(RuntimeError, match="rendering is parked"):
        capture(world, camera, 256, 1)
    world.spawn_actor.assert_not_called()
    world.apply_settings.assert_not_called()
