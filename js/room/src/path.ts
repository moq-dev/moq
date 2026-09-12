/**
 * Room path convention: a participant identity is everything before the last
 * segment, and that last segment is the broadcast kind (`camera` or `screen`).
 *
 * `alice/camera`, `alice/camera.hang`, and `guest/uuid/screen` are all valid.
 * A `.hang` suffix on the kind is optional and equivalent.
 *
 * @module
 */

import { Path } from "@moq/net";

/** The two broadcasts each participant may publish. */
export const KIND = {
	camera: "camera",
	screen: "screen",
} as const;

/** A participant broadcast kind. */
export type Kind = (typeof KIND)[keyof typeof KIND];

/** An announced path split into identity and kind, or `undefined` if it is not a room broadcast. */
export type Parsed = {
	/** Path prefix identifying the participant; may be more than one segment. */
	identity: Path.Valid;
	/** `camera` (camera + mic) or `screen` (screenshare). */
	kind: Kind;
};

/** True when `value` is a {@link Kind}. */
export function isKind(value: string): value is Kind {
	return value === KIND.camera || value === KIND.screen;
}

/**
 * Strip an optional `.hang` catalog-format suffix from a path segment.
 *
 * `camera` and `camera.hang` are the same kind; publishers may omit the suffix
 * (hang.live does) because hang is the default catalog format.
 */
export function kindFromSegment(segment: string): Kind | undefined {
	const kind = segment.endsWith(".hang") ? segment.slice(0, -".hang".length) : segment;
	return isKind(kind) ? kind : undefined;
}

/**
 * Split a room-relative broadcast path into identity and kind.
 *
 * The last segment is the kind; everything before it is the identity. Returns
 * `undefined` when there is no identity, or the last segment is not a kind.
 */
export function parse(path: Path.Valid): Parsed | undefined {
	const parts = Path.parts(path);
	if (parts.length < 2) return undefined;

	const kind = kindFromSegment(parts[parts.length - 1]);
	if (!kind) return undefined;

	return { identity: Path.from(...parts.slice(0, -1)), kind };
}

/** The broadcast path a participant publishes for `kind`. */
export function broadcastPath(identity: Path.Valid, kind: Kind): Path.Valid {
	return Path.join(identity, Path.from(kind));
}
