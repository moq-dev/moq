import time
from types import SimpleNamespace

import pytest

from moq_carla_sim.benchmark import Frames, main, measure, require_idle, summary


def test_delivery_uses_frame_identity_not_callback_count():
    frames = Frames()
    started = time.monotonic()
    for number in (9, 10, 10, 12, 13):
        frames.receive(SimpleNamespace(frame=number))
    result = frames.finish(dict.fromkeys((10, 11, 12), started), time.monotonic())
    assert (result["delivered"], result["missing"], result["duplicates"]) == (2, [11], 1)
    assert result["callback_delay_max_ms"] >= 0


def test_camera_without_callbacks_reports_failure():
    result = Frames().finish({10: time.monotonic()}, time.monotonic())
    assert result["delivered"] == 0
    assert result["missing"] == [10]
    assert result["callback_delay_p95_ms"] is None


def test_tick_rate_does_not_hide_tail_stalls():
    result = summary([0.01] * 19 + [0.1])
    assert result["tick_fps"] > 30
    assert result["over_30fps_budget"] == 1
    assert result["max_ms"] == 100
    assert result["p95_ms"] == 10


@pytest.mark.parametrize("synchronous,actors", [(True, []), (False, [object()])])
def test_refuses_an_owned_world(synchronous, actors):
    world = SimpleNamespace(
        get_settings=lambda: SimpleNamespace(synchronous_mode=synchronous),
        wait_for_tick=lambda _: None,
        get_actors=lambda: SimpleNamespace(filter=lambda _: actors),
    )
    with pytest.raises(RuntimeError, match="benchmark requires"):
        require_idle(world, 1)


def test_waits_for_actor_snapshot_before_accepting_empty_world():
    actors = []
    world = SimpleNamespace(
        get_settings=lambda: SimpleNamespace(synchronous_mode=False),
        wait_for_tick=lambda _: actors.append(object()),
        get_actors=lambda: SimpleNamespace(filter=lambda _: actors),
    )
    with pytest.raises(RuntimeError, match="empty server"):
        require_idle(world, 1)


def test_spawn_failure_restores_clock_even_when_actor_cleanup_fails():
    events = []
    blueprint = SimpleNamespace(set_attribute=lambda *_: None)

    def spawn(*args, **kwargs):
        if "attach_to" in kwargs:
            raise RuntimeError("sensor spawn failed")
        return SimpleNamespace(
            id=1, set_simulate_physics=lambda _: None, destroy=lambda: events.append("destroy vehicle") or False
        )

    world = SimpleNamespace(
        get_settings=lambda: SimpleNamespace(synchronous_mode=False, no_rendering_mode=True),
        wait_for_tick=lambda _: None,
        get_actors=lambda: SimpleNamespace(filter=lambda _: []),
        apply_settings=lambda settings: events.append((settings.synchronous_mode, settings.no_rendering_mode)),
        get_blueprint_library=lambda: SimpleNamespace(find=lambda _: blueprint),
        get_map=lambda: SimpleNamespace(get_spawn_points=lambda: [object()]),
        spawn_actor=spawn,
    )
    with pytest.raises(RuntimeError, match="failed to destroy"):
        measure(SimpleNamespace(get_world=lambda: world), 1, 320, 180, 1, 1, 1, True)
    assert events == [(True, False), "destroy vehicle", (False, True)]


@pytest.mark.parametrize("args", [["--ticks", "0"], ["--timeout", "nan"], ["--sizes", "1280"], ["--cameras", "-1"]])
def test_rejects_invalid_inputs_before_connecting(args):
    with pytest.raises(SystemExit) as error:
        main(args)
    assert error.value.code == 2
