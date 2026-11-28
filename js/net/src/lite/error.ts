import { closeReason, ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import { isDecodeError } from "../util/decode_error.ts";

// Control handlers share AUTH's session-close policy; data readers never call this. Pass only an
// error from this session's own input: a served track's error may carry another session's verdict.
export function closeViolation(quic: WebTransport, cause: unknown): ProtocolViolation | undefined {
	const err =
		isDecodeError(cause) && !(cause instanceof ProtocolViolation)
			? new ProtocolViolation("invalid control encoding", { cause })
			: cause;
	if (!(err instanceof ProtocolViolation)) return undefined;
	quic.close({ closeCode: err.code, reason: closeReason(err.message) });
	return err;
}

/**
 * A served track's or group's error as its subscriber reads it. A violation it carries is another
 * session's, which leaves this one open: a reset saying SESSION_CLOSED would send a viewer waiting
 * on a route change, so it reads as a malformed track instead.
 */
export function servedError<E>(err: E): E | StreamError {
	if (!(err instanceof ProtocolViolation)) return err;
	return new StreamError(StreamCode.MalformedTrack, { message: err.message, cause: err });
}
