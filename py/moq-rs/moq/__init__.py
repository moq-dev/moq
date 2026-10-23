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
    BroadcastPrepared,
    BroadcastRequest,
    OriginConsumer,
    OriginDynamic,
    OriginProducer,
)
from .publish import (
    BroadcastDynamic,
    BroadcastFinish,
    BroadcastProducer,
    GroupProducer,
    GroupRequest,
    TrackDemand,
    TrackDynamic,
    TrackProducer,
    TrackRequest,
)
from .server import Request, Server, Transport
from .session import Auth, AuthToken, Bandwidth, Reservation, Session
from .subscribe import (
    BroadcastConsumer,
    GroupConsumer,
    TrackConsumer,
)
from .types import (
    Anchor,
    AnnounceRoute,
    AudioCodec,
    AudioEncoderInput,
    AudioSampleFormat,
    Backoff,
    ClientTls,
    ConnectionStats,
    ConnectionStatus,
    Datagram,
    ErrorScope,
    FetchGroupOptions,
    Frame,
    Grant,
    Order,
    ProtocolError,
    ProtocolKind,
    QuicConfig,
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
    WebSocketConfig,
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
    "AnnounceRoute",
    "AudioCodec",
    "AudioEncoderInput",
    "AudioSampleFormat",
    "Auth",
    "AuthToken",
    "Backoff",
    "Bandwidth",
    "BroadcastConsumer",
    "BroadcastDynamic",
    "BroadcastFinish",
    "BroadcastPrepared",
    "BroadcastProducer",
    "BroadcastRequest",
    "Client",
    "ClientTls",
    "CongestionControl",
    "ConnectionStats",
    "ConnectionStatus",
    "Datagram",
    "Error",
    "ErrorScope",
    "Frame",
    "ProtocolError",
    "ProtocolKind",
    "QuicConfig",
    "FetchGroupOptions",
    "Grant",
    "GroupConsumer",
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
    "WebSocketConfig",
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
