import type { Reader, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import { type IetfVersion, Version } from "./version.ts";

// TIMESCALE (0x08), from the MOQ Properties registry shared with draft-ietf-moq-loc-04.
// Track scope: it declares the units of every object Timestamp on the track, and its
// presence is what opts the track into timestamps at all.
const TIMESCALE = 0x08n;

/// Write the track's Timescale as a Track Property block.
///
/// Track Properties are the final field of the message, so this writes the block with
/// no count and no length; the caller must not append anything after it.
export async function encode(w: Writer, timescale: Timescale, version: IetfVersion): Promise<void> {
	// Track Properties only exist in draft-17+; older drafts have nowhere to put this.
	if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
		return;
	}

	// First property in the block, so the delta-encoded type is the absolute id.
	await w.u62(TIMESCALE);
	await w.u62(BigInt(timescale));
}

/// Parse Track Properties from the remaining bytes of a message, returning the track's
/// Timescale if it declared one.
///
/// Track Properties are delta-encoded Key-Value-Pairs (same format as
/// Message Parameters) but with NO count prefix — they extend to the
/// end of the message payload.
///
/// `undefined` means the track declared no timeline, which is not an error: the caller
/// times its objects by arrival instead.
///
/// Only present in draft-17+; older drafts don't have Track Properties.
export async function decode(r: Reader, version: IetfVersion): Promise<Timescale | undefined> {
	// Track Properties only exist in draft-17+
	if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
		return undefined;
	}

	let timescale: Timescale | undefined;
	let prevType = 0n;
	let i = 0;

	while (!(await r.done())) {
		const delta = await r.u62();
		const abs = i === 0 ? delta : prevType + delta;
		prevType = abs;
		i++;

		if (abs % 2n === 0n) {
			// Even type: single varint value
			const value = await r.u62();
			if (abs === TIMESCALE && value > 0n) {
				// A zero timescale is invalid; treat it as no declaration rather than
				// failing the whole message over one property we could have ignored.
				timescale = Timescale(Number(value));
			}
		} else {
			// Odd type: length-prefixed bytes
			const len = await r.u53();
			await r.read(len);
		}
	}

	return timescale;
}
