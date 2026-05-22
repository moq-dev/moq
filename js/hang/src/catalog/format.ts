// Filename-style format extensions for broadcast names.
//
// Broadcast names use a filename-style suffix to advertise their catalog format,
// e.g. `demo/bbb.hang` or `demo/bbb.msf`. Consumers parse the suffix to pick a
// catalog track without explicit configuration; publishers should include the
// suffix in the name they publish so consumers can detect it.

export const FORMATS = ["hang", "msf"] as const;
export type Format = (typeof FORMATS)[number];

export const DEFAULT_FORMAT: Format = "hang";

/** Detect the catalog format from a broadcast name suffix, or `undefined` if the name has no recognized extension. */
export function detectFormat(name: string): Format | undefined {
	for (const format of FORMATS) {
		if (name.endsWith(`.${format}`)) return format;
	}
	return undefined;
}
