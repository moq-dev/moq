import * as z from "@zod/mini";
import { u53Schema } from "./integers";
import { MOQ_EPOCH_UNIX_MILLIS } from "./timeline";

/** Units per second for a catalog clock. Matches Rust `u32`; zero is refused. */
const clockTimescaleSchema = z.number().check(z.int(), z.positive(), z.lte(4_294_967_295));

/**
 * The broadcast's one continuous clock, advertised at the catalog root.
 *
 * `wall` is the wall-clock time of PTS zero, in `timescale` units since the moq epoch
 * ({@link MOQ_EPOCH_UNIX_MILLIS}, 2020-01-01). Every media track and the archive index refer
 * to this one mapping after timescale conversion, so there are no competing wall epochs. The
 * publisher fixes it once and never overwrites it: a discontinuity marker is a delivery event,
 * not a new epoch, and a system-clock adjustment never retimes it. It is independent of the
 * `archive` entry: a live-only publisher exposes its clock without creating a segment index.
 */
export const ClockSchema = z.object({
	// The wall-clock time of PTS zero, in `timescale` units since the moq epoch. Must fit in a
	// JSON-safe integer so browsers read back exactly what the publisher wrote.
	wall: u53Schema,

	// Units per second for `wall`. Defaults to 1_000_000 (microseconds), the broadcast clock's
	// own timescale. Zero is refused: no timestamp can be expressed in it. Bounded to u32 so
	// Rust and JS accept the same catalogs.
	timescale: z._default(clockTimescaleSchema, 1_000_000),
});

/** The broadcast's one continuous clock. */
export type Clock = z.infer<typeof ClockSchema>;

/**
 * The wall-clock time of `pts`, given in `ptsTimescale` units per second, under the broadcast's
 * fixed clock mapping (`wall + pts` after conversion into the clock's timescale).
 *
 * Throws on a zero timescale, a non-integer or unsafe value, or a result outside the JSON-safe
 * integer range, rather than truncating.
 */
export function wallClockTime(clock: Clock, pts: number, ptsTimescale: number): Date {
	if (!Number.isSafeInteger(clock.wall)) throw new RangeError(`invalid wall clock: ${clock.wall}`);
	if (!Number.isSafeInteger(clock.timescale) || clock.timescale <= 0 || clock.timescale > 4_294_967_295)
		throw new RangeError(`invalid timescale: ${clock.timescale}`);
	if (!Number.isSafeInteger(pts) || pts < 0) throw new RangeError(`invalid pts: ${pts}`);
	if (!Number.isSafeInteger(ptsTimescale) || ptsTimescale <= 0)
		throw new RangeError(`invalid timescale: ${ptsTimescale}`);

	const units = Math.floor((pts * clock.timescale) / ptsTimescale);
	const total = clock.wall + units;
	if (!Number.isSafeInteger(total)) throw new RangeError(`invalid wall clock: ${total}`);

	const unixMillis = MOQ_EPOCH_UNIX_MILLIS + Math.floor((total * 1000) / clock.timescale);
	return new Date(unixMillis);
}
