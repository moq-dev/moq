import type * as Catalog from "../catalog";

/**
 * Builds an MP4 MIME type string for MediaSource from a codec string.
 *
 * @param codec - The codec string from the catalog (e.g., "avc1.42E01E", "mp4a.40.2")
 * @param type - "video" or "audio"
 * @returns MP4 MIME type string (e.g., "video/mp4; codecs=\"avc1.42E01E\"")
 */
function buildMp4MimeType(codec: string, type: "video" | "audio"): string {
	// For MP4 containers, we use the standard MIME type format
	// Most codecs are already in the correct format for MSE
	return `${type}/mp4; codecs="${codec}"`;
}

/**
 * Checks if a MIME type is supported by MediaSource.
 *
 * @param mimeType - The MIME type to check
 * @returns true if supported, false otherwise
 */
export function isMimeTypeSupported(mimeType: string): boolean {
	return MediaSource.isTypeSupported(mimeType);
}

/**
 * Builds and validates an MP4 MIME type for video from catalog config.
 *
 * @param config - Video configuration from catalog
 * @returns MP4 MIME type string or undefined if not supported
 */
export function buildMp4VideoMimeType(config: Catalog.VideoConfig): string | undefined {
	const mimeType = buildMp4MimeType(config.codec, "video");
	if (isMimeTypeSupported(mimeType)) {
		return mimeType;
	}
	return undefined;
}

/**
 * Builds and validates an MP4 MIME type for audio from catalog config.
 *
 * @param config - Audio configuration from catalog
 * @returns MP4 MIME type string or undefined if not supported
 */
export function buildMp4AudioMimeType(config: Catalog.AudioConfig): string | undefined {
	const mimeType = buildMp4MimeType(config.codec, "audio");
	if (isMimeTypeSupported(mimeType)) {
		return mimeType;
	}
	return undefined;
}
