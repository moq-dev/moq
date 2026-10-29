// Sample rules shared by the containers that carry ISO BMFF samples (CMAF and LOCMAF), kept out of
// the public `cmaf` entrypoint.

import type { Time } from "@moq/net";

/** `sample_is_non_sync_sample`, bit 16 of the ISO/IEC 14496-12 sample flags. */
const NON_SYNC = 0x0001_0000;

/** `sample_depends_on == 1`: the sample references others. */
const DEPENDS_ON_OTHERS = 0x0100_0000;

/**
 * Whether sample flags describe a sync sample: `sample_is_non_sync_sample` clear and not declared to
 * reference other samples, the rule ffmpeg applies and `moq-mux` mirrors. Flags that say nothing
 * (zero) describe a sync sample.
 */
export function isSync(flags: number): boolean {
	return (flags & (NON_SYNC | DEPENDS_ON_OTHERS)) === 0;
}

/**
 * The microsecond presentation time of each sample, given its `ptss` in media ticks.
 *
 * The earliest sample presents at the moq-net `timestamp`; the ticks only place the samples
 * relative to it. An untimed frame (`undefined`) presents at the ticks themselves.
 */
export function present(ptss: number[], timestamp: Time.Timestamp | undefined, timescale: number): number[] {
	// A loop, not `Math.min(...ptss)`: a long fragment's sample count can exceed the argument limit.
	let earliest = 0;
	if (timestamp !== undefined) {
		earliest = Number.POSITIVE_INFINITY;
		for (const pts of ptss) earliest = Math.min(earliest, pts);
	}
	const anchor = timestamp?.asMicros() ?? 0;
	return ptss.map((pts) => Math.round(anchor + ((pts - earliest) * 1_000_000) / timescale));
}
