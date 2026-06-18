"""The networking layer for Media over QUIC.

Real-time pub/sub with built-in caching, fan-out, and prioritization.
"""

from moq_ffi import MoqError as Error

from .client import Client, connect
from .log import log_level
from .origin import Announced, AnnouncedBroadcast, Announcement, OriginConsumer, OriginProducer
from .publish import (
    AudioProducer,
    BroadcastDynamic,
    BroadcastProducer,
    GroupProducer,
    MediaProducer,
    MediaStreamProducer,
    TrackProducer,
    TrackRequest,
)
from .server import Request, Server, Transport
from .session import Session
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
    Container,
    Dimensions,
    Frame,
    Subscription,
    TrackInfo,
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
    "Error",
    "Frame",
    "GroupConsumer",
    "GroupProducer",
    "MediaConsumer",
    "MediaProducer",
    "MediaStreamProducer",
    "OriginConsumer",
    "OriginProducer",
    "Request",
    "Server",
    "Session",
    "Subscription",
    "TrackConsumer",
    "TrackInfo",
    "TrackProducer",
    "TrackRequest",
    "Transport",
    "Video",
    "connect",
    "log_level",
]
