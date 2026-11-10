import * as Catalog from "@moq/hang/catalog";
import * as Util from "@moq/hang/util";
import { base64ToBytes } from "../base64";
import { supportCacheKey } from "../support-probe";
import { decoderConfigKey, hardwareAcceleration } from "./config";

/** Whether the WebCodecs video decoder can play this config. */
export async function supported(config: Catalog.VideoConfig): Promise<boolean> {
	return (await accepted(config)) !== undefined;
}

/**
 * The codec string WebCodecs accepts for this config, or undefined when none fits. It never rewrites
 * the config, so the catalog keeps the publisher's string while the decoder configures with this one.
 */
export async function accepted(config: Catalog.VideoConfig): Promise<string | undefined> {
	if (!Catalog.containerSupported(config.container)) {
		// `kind` is the literal "unknown" tag; the container the publisher actually named is in `raw`.
		const kind = config.container.kind === "unknown" ? config.container.raw.kind : config.container.kind;
		console.warn(`video: ignoring rendition with unknown container: ${kind}`);
		return undefined;
	}

	let description: Uint8Array | undefined;
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
			// description-less config and then having runCmaf fail later.
			console.warn(`video: malformed CMAF init segment for codec ${config.codec}`, err);
			return undefined;
		}
	}

	// Safari rejects `avc3.*` codec strings even though its H.264 decoder handles inline SPS/PPS,
	// so retry those as `avc1.*`.
	const codecs = [config.codec];
	if (config.codec.startsWith("avc3.")) codecs.push(`avc1.${config.codec.slice("avc3.".length)}`);

	for (const codec of codecs) {
		const { supported } = await VideoDecoder.isConfigSupported({
			codec,
			description,
			displayAspectWidth: config.displayAspectWidth,
			displayAspectHeight: config.displayAspectHeight,
			optimizeForLatency: config.optimizeForLatency ?? true,
			hardwareAcceleration: hardwareAcceleration(config),
		});
		if (supported) return codec;
	}
	return undefined;
}

Object.assign(supported, { [supportCacheKey]: decoderConfigKey });
