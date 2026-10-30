"""Lightweight stand-ins for runtime-only modules used during unit collection."""

import sys
import uuid
from types import ModuleType, SimpleNamespace

import pytest


class MoqError(Exception):
    pass


class AnnounceEvent:
    """One announcement event; the subclass names which kind, as in moq's."""

    def __init__(self, announce: SimpleNamespace) -> None:
        self.announce = announce


class AnnounceEventStart(AnnounceEvent):
    pass


class AnnounceEventUpdate(AnnounceEvent):
    pass


class AnnounceEventEnd(AnnounceEvent):
    pass


class AnnounceEventRestart(AnnounceEvent):
    pass


class SnapshotProducer:
    """Records every published value; demand comes from the wrapped track."""

    def __init__(self, broadcast: object, track: SimpleNamespace) -> None:
        self.track = track
        self.values: list[object] = []

    def demand(self) -> object:
        return self.track.demand()

    def update(self, value: object) -> None:
        self.values.append(value)


class SnapshotConsumer:
    """Yields whatever the wrapped fake track yields, already parsed."""

    def __init__(self, track: object) -> None:
        self._track = track

    def __aiter__(self):
        return aiter(self._track)


moq = ModuleType("moq")
moq.Error = MoqError
moq.AnnounceEventStart = AnnounceEventStart
moq.AnnounceEventUpdate = AnnounceEventUpdate
moq.AnnounceEventEnd = AnnounceEventEnd
moq.AnnounceEventRestart = AnnounceEventRestart
moq.AnnounceRoute = lambda **kwargs: SimpleNamespace(**kwargs)
moq.mint_epoch = lambda: str(uuid.uuid4())
moq.TrackInfo = lambda **kwargs: SimpleNamespace(**kwargs)

moq_json = ModuleType("moq.json")
moq_json.SnapshotProducer = SnapshotProducer
moq_json.SnapshotConsumer = SnapshotConsumer
moq.json = moq_json

moq_media = ModuleType("moq.media")
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
