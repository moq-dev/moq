import { Path } from "@moq/net";

/** The prefix a relay publishes its stats broadcasts under by default. */
export const PREFIX = ".stats";

const PRESENCE = "presence.json";
const PREFIX_TRACK = "prefix.json";
const BUCKET = /^bucket\.([^/]+)\.json$/;

/** `<key>/<kind>`, or `<kind>` for the empty key. */
function keyed(key: string, kind: string): string {
	return key === "" ? kind : `${Path.from(key)}/${kind}`;
}

/** The totals track's name. */
export function totalsTrack(): string {
	return "totals.json";
}

/** The presence track for a stats label: `<label>/presence.json`, served only while requested. */
export function presenceTrack(label: string): string {
	return keyed(label, PRESENCE);
}

/** The prefix track for a broadcast prefix: `<path>/prefix.json`, served only while requested. */
export function prefixTrack(path: string): string {
	return keyed(path, PREFIX_TRACK);
}

/** The track for one counter run of a bucket: `<bucket>/bucket.<epoch>.json`. */
export function bucketTrack(bucket: string, epoch: string): string {
	return keyed(bucket, `bucket.${epoch}.json`);
}

/** The bucket and run a bucket track name carries, or `undefined` for any other name. */
export function parseBucketTrack(name: string): { bucket: string; epoch: string } | undefined {
	const slash = name.lastIndexOf("/");
	const bucket = slash < 0 ? "" : name.slice(0, slash);
	// Only a bucket with no empty segment round-trips, so each bucket has exactly one name.
	if (slash >= 0 && bucket.split("/").includes("")) return undefined;
	const epoch = BUCKET.exec(name.slice(slash + 1))?.[1];
	return epoch ? { bucket, epoch } : undefined;
}

/**
 * The node a stats broadcast path names: `<prefix>/node/<node>` yields `<node>`, `<prefix>/node` the
 * empty node, and any other path `undefined`.
 */
export function parseNodePath(prefix: Path.Valid, path: Path.Valid): Path.Valid | undefined {
	const rest = Path.stripPrefix(prefix, path);
	if (rest === null) return undefined;
	return Path.stripPrefix(Path.from("node"), rest) ?? undefined;
}
