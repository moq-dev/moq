import type { Reader, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import { toNumber } from "../util/bigint.ts";
import { hasTimestampObjectIds, type IetfVersion } from "./version.ts";

// Legacy timescale property (0x08), emitted on draft-18..22 for side-by-side rollout compatibility
// with released moq-net-v0.3.24 peers (which use dense ordinal Object IDs). On draft-23+, 0x08 is
// LOC's TIMESCALE and passes through untouched while ID_TIMESCALE (0x40B70) carries moq-timestamp.
// Temporary migration shim: delete when draft-22 compatibility is retired.
const LEGACY_TIMESCALE = 0x08n;

// ID_TIMESCALE (0x40B70), from draft-lcurley-moq-timestamp.
// Track scope: it declares the units of the track's Group IDs and Object IDs (`dts =
// Group ID + Object ID - TIMESTAMP_TIE`), and its presence is what makes the track timed.
// Distinct from LOC's TIMESCALE (0x08), which is skipped as an unknown property on draft-23+.
const TIMESCALE = 0x40b70n;

// DEFAULT_PUBLISHER_GROUP_ORDER (0x22), the publisher's delivery preference for the track.
// It shares its number with the GROUP_ORDER *message parameter*, which is a different
// registry: that one is only legal in SUBSCRIBE, PUBLISH_OK, and FETCH, where the
// subscriber states its own preference.
const DEFAULT_PUBLISHER_PRIORITY = 0x0en;
const DEFAULT_PUBLISHER_GROUP_ORDER = 0x22n;

/**
 * DEFAULT_PUBLISHER_GROUP_ORDER's descending value: the newest group first, which is what
 * we serve, matching moq-lite.
 */
export const DESCENDING = 0x02;

/// The Track Properties block carried at the end of SUBSCRIBE_OK, PUBLISH, and FETCH_OK.
export interface Properties {
	/** The publisher's cache window in milliseconds, omitted when unconstrained. */
	maxCacheDuration?: bigint;

	/// The track's Timescale, which declares the units of its Group IDs.
	///
	/// `undefined` declares no timeline, so the track and its objects are untimed.
	timescale?: Timescale;

	/// Publisher priority for a group header without its priority flag. Omitted, it is the wire default 128, priority -1.
	priority?: number;

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
	let prevType = 0n;
	const sparseIds = hasTimestampObjectIds(version);

	if (properties.maxCacheDuration !== undefined) {
		await w.u62(4n);
		await w.u62(properties.maxCacheDuration);
		prevType = 4n;
	}

	if (!sparseIds && properties.timescale !== undefined) {
		await w.u62(LEGACY_TIMESCALE - prevType);
		await w.u62(BigInt(properties.timescale));
		prevType = LEGACY_TIMESCALE;
	}

	if (properties.priority !== undefined) {
		if (!Number.isInteger(properties.priority) || properties.priority < 0 || properties.priority > 255) {
			throw new RangeError(`invalid publisher priority: ${properties.priority}`);
		}
		await w.u62(DEFAULT_PUBLISHER_PRIORITY - prevType);
		await w.u62(BigInt(properties.priority));
		prevType = DEFAULT_PUBLISHER_PRIORITY;
	}

	if (properties.groupOrder !== undefined) {
		await w.u62(DEFAULT_PUBLISHER_GROUP_ORDER - prevType);
		await w.u62(BigInt(properties.groupOrder));
		prevType = DEFAULT_PUBLISHER_GROUP_ORDER;
	}

	if (sparseIds && properties.timescale !== undefined) {
		await w.u62(TIMESCALE - prevType);
		await w.u62(BigInt(properties.timescale));
		prevType = TIMESCALE;
	}
}

/// Parse Track Properties from the remaining bytes of a message.
///
/// Track Properties are delta-encoded Key-Value-Pairs (same format as
/// Message Parameters) but with NO count prefix: they extend to the
/// end of the message payload.
///
/// Unlike an unknown message parameter, an unknown property is skipped rather than fatal,
/// which is what lets a relay forward properties it does not implement.
export async function decode(r: Reader, version: IetfVersion): Promise<Properties> {
	const properties: Properties = {};
	const sparseIds = hasTimestampObjectIds(version);
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
			if (abs === 4n) {
				properties.maxCacheDuration = value;
			} else if (abs === TIMESCALE && value > 0n) {
				// A zero timescale is invalid; treat it as no declaration rather than
				// failing the whole message over one property we could have ignored.
				properties.timescale = Timescale(toNumber(value));
			} else if (!sparseIds && abs === LEGACY_TIMESCALE && value > 0n) {
				properties.timescale = Timescale(toNumber(value));
			} else if (abs === DEFAULT_PUBLISHER_PRIORITY) {
				if (value > 255n) throw new Error(`invalid publisher priority: ${value}`);
				properties.priority = Number(value);
			} else if (abs === DEFAULT_PUBLISHER_GROUP_ORDER) {
				// Only Ascending (0x1) and Descending (0x2) are defined here. Unlike the draft-14
				// fields, 0x0 has no "publisher decides" meaning to fall back on.
				if (value < 1n || value > 2n) {
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
