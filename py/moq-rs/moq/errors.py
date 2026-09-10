"""Classification helpers for the errors raised across the FFI boundary."""

from __future__ import annotations

from moq_ffi import MoqError, MoqProtocolError, MoqProtocolKind


def is_shutdown(err: BaseException) -> bool:
    """True for `Cancelled` and `Closed`, which arise from graceful shutdown
    rather than actual failures.

    Useful for breaking out of an `async for` without treating the expected
    end-of-stream error as a problem.
    """
    return isinstance(err, (MoqError.Cancelled, MoqError.Closed))


def is_auth(err: BaseException) -> bool:
    """True for HTTP 401/403 and a protocol Unauthorized session close.

    Unlike a transport failure, retrying without new credentials won't help, so
    callers should surface these rather than reconnect.
    """
    if isinstance(err, (MoqError.Unauthorized, MoqError.Forbidden)):
        return True
    protocol = protocol_error(err)
    return protocol is not None and protocol.kind == MoqProtocolKind.UNAUTHORIZED


def protocol_error(err: BaseException) -> MoqProtocolError | None:
    """The structured protocol failure, or None if `err` is not one.

    A protocol error carries the peer's session or stream scope, the verbatim
    wire code, a known kind when recognized, and a diagnostic message.
    """
    if isinstance(err, MoqError.Protocol):
        return err.details
    return None
