"""The networking layer for Media over QUIC.

Real-time pub/sub with built-in caching, fan-out, and prioritization.
"""

from moq_ffi import Container
from moq_ffi import MoqSession as Session

from .client import Client
from .origin import Announced, AnnouncedBroadcast, Announcement, OriginConsumer, OriginProducer
from .publish import AudioProducer, BroadcastDynamic, BroadcastProducer, GroupProducer, MediaProducer, TrackProducer
from .server import Request, Server, Transport
from .subscribe import (
    AudioConsumer,
    BroadcastConsumer,
    CatalogConsumer,
    GroupConsumer,
    MediaConsumer,
    TrackConsumer,
)
from .types import (
    Audio,
    AudioCodec,
    AudioDecoderOutput,
    AudioEncoderInput,
    AudioEncoderOutput,
    AudioFormat,
    AudioFrame,
    Catalog,
    Dimensions,
    Frame,
    Video,
)

__all__ = [
    "Announced",
    "AnnouncedBroadcast",
    "Announcement",
    "Audio",
    "AudioCodec",
    "AudioConsumer",
    "AudioDecoderOutput",
    "AudioEncoderInput",
    "AudioEncoderOutput",
    "AudioFormat",
    "AudioFrame",
    "AudioProducer",
    "BroadcastConsumer",
    "BroadcastDynamic",
    "BroadcastProducer",
    "Catalog",
    "CatalogConsumer",
    "Client",
    "Container",
    "Dimensions",
    "Frame",
    "GroupConsumer",
    "GroupProducer",
    "MediaConsumer",
    "MediaProducer",
    "OriginConsumer",
    "OriginProducer",
    "Request",
    "Server",
    "Session",
    "TrackConsumer",
    "TrackProducer",
    "Transport",
    "Video",
]
