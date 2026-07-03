// Filename-style format extensions for broadcast names.
//
// Broadcast names use a filename-style suffix to advertise their catalog format,
// e.g. `demo/bbb.hang` or `demo/bbb.msf`. Consumers parse the suffix to pick a
// catalog track without explicit configuration; publishers should include the
// suffix in the name they publish so consumers can detect it.

/** Track name for the uncompressed hang catalog (the `.json` track). */
export const TRACK = "catalog.json";

/** Track name for the DEFLATE-compressed hang catalog: the `.z` sibling of {@link TRACK}. */
export const TRACK_COMPRESSED = "catalog.json.z";

/** Recognized catalog format suffixes used in broadcast names. */
export const FORMATS = ["hang", "msf"] as const;
/** A catalog format advertised by a broadcast name suffix. */
export type Format = (typeof FORMATS)[number];

/** The catalog format assumed when a broadcast name has no recognized suffix. */
export const DEFAULT_FORMAT: Format = "hang";

/** Detect the catalog format from a broadcast name suffix, or `undefined` if the name has no recognized extension. */
export function detectFormat(name: string): Format | undefined {
	for (const format of FORMATS) {
		if (name.endsWith(`.${format}`)) return format;
	}
	return undefined;
}
