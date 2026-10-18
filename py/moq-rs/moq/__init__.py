"""The networking layer for Media over QUIC.

Real-time pub/sub with built-in caching, fan-out, and prioritization.
"""

from moq_ffi import MoqError as Error

from . import audio as audio

# Layer namespaces stay out of __all__; json would shadow the stdlib module.
from . import flate as flate
from . import json as json
from . import video as video
from .client import Client, CongestionControl, connect, qlog_supported, versions
from .epoch import epoch_time, mint_epoch
from .errors import is_auth, is_shutdown, protocol_error
from .log import log_level
from .origin import (
    Announce,
    AnnounceConsumer,
    AnnouncedBroadcast,
    AnnounceEvent,
    AnnounceEventEnd,
    AnnounceEventRestart,
    AnnounceEventStart,
    AnnounceEventUpdate,
    BroadcastRequest,
    OriginConsumer,
    OriginDynamic,
    OriginProducer,
)
from .publish import (
    BroadcastDynamic,
    BroadcastProducer,
    GroupDemand,
    GroupProducer,
    GroupRequest,
    TrackDemand,
    TrackDynamic,
    TrackProducer,
    TrackRequest,
)
from .server import Request, Server, Transport
from .session import Bandwidth, Reservation, Session
from .subscribe import (
    BroadcastConsumer,
    GroupConsumer,
    TrackConsumer,
)
from .types import (
    Anchor,
    AudioCodec,
    AudioEncoderInput,
    AudioSampleFormat,
    Backoff,
    ConnectionStats,
    ConnectionStatus,
    Datagram,
    ErrorScope,
    FetchGroupOptions,
    Frame,
    Order,
    ProtocolError,
    ProtocolKind,
    Range,
    Route,
    Subscription,
    TrackInfo,
    VideoCodec,
    VideoDecodedFrame,
    VideoEncoderInput,
    VideoEncoderKind,
    VideoEncoderOutput,
    VideoPixelFormat,
)

__all__ = [
    "Anchor",
    "Announce",
    "AnnounceConsumer",
    "AnnouncedBroadcast",
    "AnnounceEvent",
    "AnnounceEventEnd",
    "AnnounceEventRestart",
    "AnnounceEventStart",
    "AnnounceEventUpdate",
    "AudioCodec",
    "AudioEncoderInput",
    "AudioSampleFormat",
    "Backoff",
    "Bandwidth",
    "BroadcastConsumer",
    "BroadcastDynamic",
    "BroadcastProducer",
    "BroadcastRequest",
    "Client",
    "CongestionControl",
    "ConnectionStats",
    "ConnectionStatus",
    "Datagram",
    "Error",
    "ErrorScope",
    "Frame",
    "ProtocolError",
    "ProtocolKind",
    "FetchGroupOptions",
    "GroupConsumer",
    "GroupDemand",
    "GroupRequest",
    "GroupProducer",
    "OriginConsumer",
    "OriginDynamic",
    "OriginProducer",
    "Order",
    "Range",
    "Request",
    "Reservation",
    "Route",
    "Server",
    "Session",
    "Subscription",
    "TrackConsumer",
    "TrackDemand",
    "TrackDynamic",
    "TrackInfo",
    "TrackProducer",
    "TrackRequest",
    "Transport",
    "VideoCodec",
    "VideoEncoderInput",
    "VideoEncoderKind",
    "VideoEncoderOutput",
    "VideoDecodedFrame",
    "VideoPixelFormat",
    "connect",
    "epoch_time",
    "is_auth",
    "is_shutdown",
    "log_level",
    "mint_epoch",
    "protocol_error",
    "qlog_supported",
    "versions",
]

from . import media as media
