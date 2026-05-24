"""The networking layer for Media over QUIC.

Real-time pub/sub with built-in caching, fan-out, and prioritization.
"""

from ._uniffi import MoqSession as Session
from .client import Client
from .origin import Announced, AnnouncedBroadcast, Announcement, OriginConsumer, OriginProducer
from .publish import BroadcastProducer, GroupProducer, MediaProducer, TrackProducer
from .server import Request, Server, Transport
from .subscribe import BroadcastConsumer, CatalogConsumer, Container, GroupConsumer, MediaConsumer, TrackConsumer
from .types import Audio, Catalog, Dimensions, Frame, Video

__all__ = [
    "Audio",
    "Announced",
    "AnnouncedBroadcast",
    "Announcement",
    "BroadcastConsumer",
    "BroadcastProducer",
    "Catalog",
    "Container",
    "CatalogConsumer",
    "Client",
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
