/**
 * Buffered time-range types shared by the watch decoders.
 *
 * Kept in a leaf module (depends only on `@moq/net`) so the element and the
 * per-track decoders can share them without importing each other.
 *
 * @module
 */
import type * as Moq from "@moq/net";

/** A single buffered time range. */
export interface BufferedRange {
	start: Moq.Time.Milli;
	end: Moq.Time.Milli;
}

/** The media currently buffered, ordered by start time. */
export type BufferedRanges = BufferedRange[];
