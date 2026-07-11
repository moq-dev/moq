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
    GroupRequest,
    MediaProducer,
    MediaStreamProducer,
    TrackDynamic,
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
    ConnectionStats,
    Container,
    Dimensions,
    FetchGroupOptions,
    Frame,
    Subscription,
    TrackInfo,
    Video,
    VideoHint,
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
    "ConnectionStats",
    "Container",
    "Dimensions",
    "Error",
    "Frame",
    "FetchGroupOptions",
    "GroupConsumer",
    "GroupRequest",
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
    "TrackDynamic",
    "TrackInfo",
    "TrackProducer",
    "TrackRequest",
    "Transport",
    "Video",
    "VideoHint",
    "connect",
    "log_level",
]
