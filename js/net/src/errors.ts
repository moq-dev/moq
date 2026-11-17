/**
 * Session and stream error types, and what to do after one.
 *
 * @module
 */
export {
	ConnectTimeout,
	FrameTooLarge,
	GroupTooLarge,
	NotFound,
	ProtocolViolation,
	RefusedRedirect,
	Retry,
	retry,
	Session,
	Skipped,
	Stream,
	type StreamOptions,
	TimestampMismatch,
	TooFarBehind,
} from "./error.ts";
