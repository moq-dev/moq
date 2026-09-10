/**
 * Requested ends versus faults. Classification is the stream's code, not the message text.
 *
 * @module
 */

import { isCancel } from "@moq/net";

/**
 * Finish (`null`) or StreamCode.Cancel. `undefined` is still open, so a decoder
 * error is a fault until the stream says otherwise.
 */
export function isCleanEnd(end: unknown): boolean {
	return end === null || isCancel(end);
}

/**
 * Whether a WebCodecs `error` callback is a requested end rather than a decoder fault.
 *
 * A truncated group and `decoder.close()` both arrive as the same `DOMException`, so the
 * classification comes from the stream's end (and whether this effect was torn down), not
 * from the exception.
 */
export function isDecoderEnd(
	consumer: { cancelled: boolean; closed: { peek(): Error | null | undefined } },
	aborted: boolean,
): boolean {
	return aborted || consumer.cancelled || isCleanEnd(consumer.closed.peek());
}
