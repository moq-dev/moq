// Filename-style format extensions for broadcast names.
//
// Broadcast names use a filename-style suffix to advertise their catalog format,
// e.g. `demo/bbb.hang` or `demo/bbb.msf`. Producers append the suffix when missing,
// consumers parse it to pick a catalog track without explicit configuration.

export const CATALOG_FORMATS = ["hang", "msf"] as const;
export type CatalogFormat = (typeof CATALOG_FORMATS)[number];

export const DEFAULT_CATALOG_FORMAT: CatalogFormat = "hang";

const EXTENSIONS: Record<CatalogFormat, string> = {
	hang: ".hang",
	msf: ".msf",
};

export function extensionFor(format: CatalogFormat): string {
	return EXTENSIONS[format];
}

/** Detect the catalog format from a broadcast name suffix, or `undefined` if the name has no recognized extension. */
export function detectFormat(name: string): CatalogFormat | undefined {
	for (const format of CATALOG_FORMATS) {
		if (name.endsWith(EXTENSIONS[format])) return format;
	}
	return undefined;
}

/** Return `name` unchanged if it already has a recognized extension, otherwise append the fallback extension. */
export function ensureExtension(name: string, fallback: CatalogFormat = DEFAULT_CATALOG_FORMAT): string {
	if (detectFormat(name) !== undefined) return name;
	return name + EXTENSIONS[fallback];
}
