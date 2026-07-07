// Video encode codec preference lists for Encoder.#bestCodec.
//
// Intentionally not re-exported from ./index (video/index.ts), so the ordering stays an internal
// detail that unit tests can pin without widening the package's public API.

// Codec families, most-preferred profile first within each. The bare-family entries ("avc1",
// "hev1", "vp09", "av01") let the browser pick a profile/level when a specific one isn't required.
const H264 = ["avc1.640028", "avc1.4D401F", "avc1.42E01E", "avc1"]; // Almost always hardware.
const HEVC = ["hev1.1.6.L93.B0", "hev1"]; // Often hardware, but licensing limits support (no Firefox decode).
const VP9 = ["vp09.00.10.08", "vp09"]; // Broadly hardware-*decodable*; hardware *encode* is rare.
const AV1 = ["av01.0.08M.08", "av01"]; // Great compression, but hardware encode is rare and CPU encode is costly.
const VP8 = ["vp8"]; // A terrible codec, but easy and widely supported.

/**
 * Ordered codec preference for the hardware-accelerated encode pass.
 *
 * #bestCodec returns the first codec the browser reports as supported under `prefer-hardware`, so
 * this order picks the default. The generic order front-loads VP9 because it is the most widely
 * hardware-decodable, which suits the watcher side on most engines.
 *
 * Safari is the exception: it reports VP9 as supported under `prefer-hardware` even though it only
 * has a software (libvpx) VP9 encoder, so the generic order makes Safari burn CPU on software VP9
 * while its hardware H.264/HEVC (VideoToolbox) sits idle. On Safari we therefore offer only the
 * codecs it actually hardware-encodes; anything else is reached, if ever needed, via the software
 * pass. `hardwareAcceleration` is a hint the browser may ignore, so we cannot detect this from the
 * probe (see https://github.com/w3c/webcodecs/issues/896) and must key off the engine.
 */
export function hardwareCodecOrder(safari: boolean): string[] {
	if (safari) return [...H264, ...HEVC];
	return [...VP9, ...H264, ...AV1, ...HEVC, ...VP8];
}

/** Ordered codec preference for the software encode pass, cheapest-to-encode first. */
export function softwareCodecOrder(): string[] {
	return [...H264, ...VP8, ...VP9, ...HEVC, ...AV1];
}
