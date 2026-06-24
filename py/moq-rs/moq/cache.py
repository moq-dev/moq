"""Shared RAM LRU group cache: retain history beyond the latest group."""

from __future__ import annotations

from moq_ffi import MoqCache, MoqCacheConfig

from .types import CacheConfig


class Cache:
    """A shared, cheaply cloneable handle to a RAM LRU group cache.

    Attach it to a broadcast (or origin) with ``with_cache``; pass the same
    :class:`Cache` to several to pool one retention budget across them. Eviction
    is LRU by wall-clock last-access time, bounded by a byte budget and an age
    window.
    """

    def __init__(self, config: CacheConfig | None = None) -> None:
        self._inner = MoqCache(config if config is not None else MoqCacheConfig())

    @classmethod
    def _from_inner(cls, inner: MoqCache) -> Cache:
        out = cls.__new__(cls)
        out._inner = inner
        return out

    def clone_handle(self) -> Cache:
        """A handle sharing this cache's budget. Attach it elsewhere to pool retention."""
        return Cache._from_inner(self._inner.clone_handle())

    def is_clone(self, other: Cache) -> bool:
        """Whether two handles share the same underlying budget."""
        return self._inner.is_clone(other._inner)
