import type { Reader, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import { type IetfVersion, Version } from "./version.ts";

// TIMESCALE (0x08), from the MOQ Properties registry shared with draft-ietf-moq-loc-04.
// Track scope: it declares the units of every object Timestamp on the track, and its
// presence is what opts the track into timestamps at all.
const TIMESCALE = 0x08n;

// DEFAULT_PUBLISHER_GROUP_ORDER (0x22), the publisher's delivery preference for the track.
// It shares its number with the GROUP_ORDER *message parameter*, which is a different
// registry: that one is only legal in SUBSCRIBE, PUBLISH_OK, and FETCH, where the
// subscriber states its own preference.
const DEFAULT_PUBLISHER_GROUP_ORDER = 0x22n;

/// The Track Properties block carried at the end of SUBSCRIBE_OK, PUBLISH, and FETCH_OK.
export interface Properties {
	/// The track's Timescale, which declares the units of every object Timestamp on it.
	///
	/// `undefined` declares no timeline, so the subscriber times objects by arrival.
	timescale?: Timescale;

	/// The publisher's preference for prioritizing groups within a subscription,
	/// Ascending (0x1) or Descending (0x2).
	///
	/// `undefined` means the draft default, Ascending.
	groupOrder?: number;
}

/// Write the block, which is the final field of the message: no count and no length, so
/// the caller must not append anything after it.
///
/// Properties are serialized in ascending order by type, delta-encoded.
export async function encode(w: Writer, properties: Properties, version: IetfVersion): Promise<void> {
	// Track Properties only exist in draft-17+; older drafts have nowhere to put these.
	if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
		return;
	}

	let prevType = 0n;

	if (properties.timescale !== undefined) {
		await w.u62(TIMESCALE);
		await w.u62(BigInt(properties.timescale));
		prevType = TIMESCALE;
	}

	if (properties.groupOrder !== undefined) {
		await w.u62(DEFAULT_PUBLISHER_GROUP_ORDER - prevType);
		await w.u62(BigInt(properties.groupOrder));
	}
}

/// Parse Track Properties from the remaining bytes of a message.
///
/// Track Properties are delta-encoded Key-Value-Pairs (same format as
/// Message Parameters) but with NO count prefix — they extend to the
/// end of the message payload.
///
/// Unlike an unknown message parameter, an unknown property is skipped rather than fatal,
/// which is what lets a relay forward properties it does not implement.
///
/// Only present in draft-17+; older drafts don't have Track Properties.
export async function decode(r: Reader, version: IetfVersion): Promise<Properties> {
	// Track Properties only exist in draft-17+
	if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
		return {};
	}

	const properties: Properties = {};
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
				properties.timescale = Timescale(Number(value));
			} else if (abs === DEFAULT_PUBLISHER_GROUP_ORDER) {
				if (value > 2n) {
					throw new Error(`unknown group order: ${value}`);
				}
				properties.groupOrder = Number(value);
			}
		} else {
			// Odd type: length-prefixed bytes
			const len = await r.u53();
			await r.read(len);
		}
	}

	return properties;
}
