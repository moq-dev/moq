import { closeReason, ProtocolViolation } from "../error.ts";
import { isDecodeError } from "../util/decode_error.ts";

// Control handlers share AUTH's session-close policy; data readers never call this.
export function closeViolation(quic: WebTransport, cause: unknown): boolean {
	const err =
		isDecodeError(cause) && !(cause instanceof ProtocolViolation)
			? new ProtocolViolation("invalid control encoding", { cause })
			: cause;
	if (!(err instanceof ProtocolViolation)) return false;
	quic.close({ closeCode: err.code, reason: closeReason(err.message) });
	return true;
}
