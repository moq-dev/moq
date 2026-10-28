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
from .epoch import mint_epoch
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
    OriginProducer,
    OriginRequests,
    Publication,
)
from .publish import (
    BroadcastProducer,
    BroadcastRequests,
    Fill,
    GroupProducer,
    TrackDemand,
    TrackProducer,
    TrackRequest,
    TrackRequests,
)
from .server import Request, Server, Transport
from .session import Allocator, BandwidthConsumer, GrantConsumer, Reservation, Session, Tokens
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
    ConnectionPhase,
    ConnectionState,
    ConnectionStats,
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
    "Allocator",
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
    "Backoff",
    "BandwidthConsumer",
    "BroadcastConsumer",
    "BroadcastRequests",
    "BroadcastPrepared",
    "BroadcastProducer",
    "BroadcastRequest",
    "Client",
    "ClientTls",
    "CongestionControl",
    "ConnectionStats",
    "ConnectionPhase",
    "ConnectionState",
    "Datagram",
    "Error",
    "ErrorScope",
    "Frame",
    "ProtocolError",
    "ProtocolKind",
    "QuicConfig",
    "FetchGroupOptions",
    "Fill",
    "Grant",
    "GrantConsumer",
    "GroupConsumer",
    "GroupProducer",
    "OriginConsumer",
    "OriginRequests",
    "OriginProducer",
    "Order",
    "Publication",
    "Range",
    "Request",
    "Reservation",
    "Route",
    "Server",
    "Session",
    "Subscription",
    "Tokens",
    "TrackConsumer",
    "TrackDemand",
    "TrackRequests",
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
    "is_auth",
    "is_shutdown",
    "log_level",
    "mint_epoch",
    "protocol_error",
    "qlog_supported",
    "versions",
]

from . import media as media
