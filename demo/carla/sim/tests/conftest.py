"""Lightweight stand-ins for runtime-only modules used during unit collection."""

import sys
from types import ModuleType, SimpleNamespace

import pytest


class MoqError(Exception):
    pass


class SnapshotProducer:
    """Records every published value; demand comes from the wrapped track."""

    def __init__(self, broadcast: object, track: SimpleNamespace) -> None:
        self.track = track
        self.values: list[object] = []

    def demand(self) -> object:
        return self.track.demand()

    def update(self, value: object, timestamp: object) -> None:
        self.values.append(value)


moq = ModuleType("moq")
moq.Error = MoqError
moq.Subscription = lambda **kwargs: SimpleNamespace(**kwargs)
moq.Delay = lambda **kwargs: SimpleNamespace(**kwargs)
moq.Order = SimpleNamespace(ASCENDING="ascending")
moq.Compression = SimpleNamespace(DEFLATE="deflate")
# Tests that run a truck's endpoint script it in place of this.
moq.socket = ModuleType("moq.socket")
moq.Timestamp = lambda value, scale: SimpleNamespace(value=value, scale=scale)
moq.TrackInfo = lambda **kwargs: SimpleNamespace(**kwargs)

moq_json = ModuleType("moq.json")
moq_json.SnapshotProducer = SnapshotProducer
moq.json = moq_json

moq_media = ModuleType("moq.media")


class CatalogProducer:
    """A broadcast clock fixed at zero, like the runtime's shared epoch."""

    def __init__(self, broadcast: object) -> None:
        pass

    def now(self) -> SimpleNamespace:
        return SimpleNamespace(value=0, scale=1_000)


moq_media.CatalogProducer = CatalogProducer
moq_media.VideoFormat = SimpleNamespace(AVC3="avc3")
moq_media.VideoInit = lambda **kwargs: SimpleNamespace(**kwargs)
moq_media.VideoHint = lambda **kwargs: SimpleNamespace(**kwargs)
moq_media.Dimensions = lambda **kwargs: SimpleNamespace(**kwargs)
moq_media.Named = lambda name=None: SimpleNamespace(name=name)
moq.media = moq_media

sys.modules["moq"] = moq
sys.modules["moq.json"] = moq_json
sys.modules["moq.media"] = moq_media

carla = ModuleType("carla")
carla.Location = lambda **kwargs: SimpleNamespace(**kwargs)
# `carla.Transform()` with no arguments is the identity transform, which is how
# a sensor is attached at its parent actor's origin.
carla.Transform = lambda location=None, rotation=None, **_kwargs: SimpleNamespace(location=location, rotation=rotation)
carla.Vector3D = lambda **kwargs: SimpleNamespace(**kwargs)
carla.VehicleControl = lambda **kwargs: SimpleNamespace(**kwargs)
carla.LaneType = SimpleNamespace(Driving="driving")
sys.modules["carla"] = carla


@pytest.fixture(params=[False, True], ids=["ordinary", "deep-temp-root"])
def bridge_socket(tmp_path, request: pytest.FixtureRequest, monkeypatch: pytest.MonkeyPatch) -> str:
    # AF_UNIX bounds the path passed to bind/connect, not the resolved location.
    # A relative filename works even under a deep devshell/pytest temporary root.
    root = tmp_path
    if request.param:
        root = tmp_path / ("nested" * 20)  # The component alone exceeds AF_UNIX's 108-byte Linux limit.
        root.mkdir()
    monkeypatch.chdir(root)
    return "bridge.sock"
