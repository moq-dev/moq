import * as Catalog from "@moq/hang/catalog";
import * as Util from "@moq/hang/util";
import { hosted } from "../assets";
import { base64ToBytes } from "../base64";
import { supportCacheKey } from "../support-probe";
import { decoderConfigKey } from "./config";

/** Whether the WebCodecs audio decoder can play this config. */
export async function supported(config: Catalog.AudioConfig): Promise<boolean> {
	if (!Catalog.containerSupported(config.container)) {
		// `kind` is the literal "unknown" tag; the container the publisher actually named is in `raw`.
		const kind = config.container.kind === "unknown" ? config.container.raw.kind : config.container.kind;
		console.warn(`audio: ignoring rendition with unknown container: ${kind}`);
		return false;
	}

	// Opus only runs at its native rates, so a catalog advertising anything else is wrong and Safari
	// refuses to decode it. Warn rather than reject: Chrome and Firefox ignore the configured rate and
	// play these streams fine, so rejecting would silence them for a publisher they handle today.
	if (config.codec === "opus" && !Util.Opus.supportsRate(config.sampleRate)) {
		console.warn(`audio: opus advertised at ${config.sampleRate}Hz, which some browsers cannot decode`);
	}

	// Opus in CMAF uses raw packets; dOps is not a valid OGG Identification Header.
	let description: Uint8Array | undefined;
	if (config.codec !== "opus") {
		if (config.description) {
			description = Util.Hex.toBytes(config.description);
		} else if (config.container.kind === "cmaf") {
			try {
				const Cmaf = await import("@moq/hang/cmaf");
				description = Cmaf.decodeInitSegment(base64ToBytes(config.container.init)).description;
			} catch (err) {
				// A malformed init segment means we can't extract the codec
				// description, so we can't probe support reliably. Reject the
				// track rather than letting isConfigSupported pass on a
				// description-less config and then having decode() fail later.
				console.warn(`audio: malformed CMAF init segment for codec ${config.codec}`, err);
				return false;
			}
		}
	}
	// Install the WebCodecs polyfill before probing, since playback would decode through it where
	// AudioDecoder is missing (Node, Bun, older browsers). Loads nothing where WebCodecs audio is native.
	await Util.Libav.polyfill(hosted.url);
	const res = await AudioDecoder.isConfigSupported({
		...config,
		description,
	});
	return res.supported ?? false;
}

Object.assign(supported, { [supportCacheKey]: decoderConfigKey });
