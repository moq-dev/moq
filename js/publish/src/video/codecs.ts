// Codec preference lists for Encoder.#bestCodec, most preferred first.
//
// Not re-exported from ./index, so the ordering stays an internal detail that the unit test can pin
// without widening the package's public API.

// Most preferred profile first within each family. The bare entries let the browser pick a profile
// when we don't need a specific one.
const H264 = ["avc1.640028", "avc1.4D401F", "avc1.42E01E", "avc1"];
const HEVC = ["hev1.1.6.L93.B0", "hev1"];
const VP9 = ["vp09.00.10.08", "vp09"];
const AV1 = ["av01.0.08M.08", "av01"];
const VP8 = ["vp8"];

/**
 * Codecs to try during the hardware encode pass, in order.
 *
 * Safari needs its own order. It accepts every one of these under `prefer-hardware` and echoes the
 * hint straight back, but VideoToolbox only hardware-encodes H.264 and HEVC. The rest fall back to
 * software (libvpx) and cost roughly 4x the CPU, so the generic order lands Safari on software VP9
 * while its hardware H.264 sits idle. The probe can't see any of this
 * (https://github.com/w3c/webcodecs/issues/896), so offer Safari only the codecs it actually
 * accelerates and leave the others to the software pass below.
 */
export function hardwareCodecs(safari: boolean): string[] {
	if (safari) return [...H264, ...HEVC];
	return [...VP9, ...H264, ...AV1, ...HEVC, ...VP8];
}

/** Codecs to try during the software encode pass, cheapest to encode first. */
export const SOFTWARE_CODECS = [...H264, ...VP8, ...VP9, ...HEVC, ...AV1];
