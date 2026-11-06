from __future__ import annotations

from typing import Generic, TypeVar

T = TypeVar("T")


class _Stream(Generic[T]):
    """Async iterator over :meth:`_next` that ends on ``None``, and an async context manager that cancels on exit."""

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc) -> None:
        self.cancel()

    def __aiter__(self):
        return self

    async def __anext__(self) -> T:
        item = await self._next()
        if item is None:
            raise StopAsyncIteration
        return item

    async def _next(self) -> T | None:
        raise NotImplementedError

    def cancel(self) -> None:
        raise NotImplementedError
