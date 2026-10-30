/** Native browser capabilities, kept separate from publisher and player requirements. @module */

/** Which side of the media fixture an engine runs. */
export type Role = "publisher" | "player";

/** Every native capability the media harness observes. */
export type Capabilities = Record<string, boolean>;

/** Probe native APIs on a secure page without importing the media implementation. */
export async function probe(wanted: { sampleRate: number; width: number; height: number }): Promise<Capabilities> {
	const has = (name: string) => typeof (globalThis as unknown as Record<string, unknown>)[name] === "function";
	const supports = async (fn: () => Promise<{ supported?: boolean }>) => {
		try {
			return (await fn()).supported === true;
		} catch {
			return false;
		}
	};
	return {
		WebTransport: has("WebTransport"),
		WebSocket: has("WebSocket"),
		VideoDecoder: has("VideoDecoder"),
		VideoEncoder: has("VideoEncoder"),
		AudioDecoder: has("AudioDecoder"),
		AudioEncoder: has("AudioEncoder"),
		AudioWorkletNode: has("AudioWorkletNode"),
		MediaStreamTrackProcessor: has("MediaStreamTrackProcessor"),
		"canvas.captureStream": typeof HTMLCanvasElement.prototype.captureStream === "function",
		"encode avc1.42001f": await supports(() =>
			VideoEncoder.isConfigSupported({ codec: "avc1.42001f", width: wanted.width, height: wanted.height }),
		),
		"decode avc1.42001f": await supports(() =>
			VideoDecoder.isConfigSupported({
				codec: "avc1.42001f",
				codedWidth: wanted.width,
				codedHeight: wanted.height,
			}),
		),
		"encode vp8": await supports(() =>
			VideoEncoder.isConfigSupported({ codec: "vp8", width: wanted.width, height: wanted.height }),
		),
		"decode vp8": await supports(() =>
			VideoDecoder.isConfigSupported({ codec: "vp8", codedWidth: wanted.width, codedHeight: wanted.height }),
		),
		"encode opus": await supports(() =>
			AudioEncoder.isConfigSupported({ codec: "opus", sampleRate: wanted.sampleRate, numberOfChannels: 1 }),
		),
		"decode opus": await supports(() =>
			AudioDecoder.isConfigSupported({ codec: "opus", sampleRate: wanted.sampleRate, numberOfChannels: 1 }),
		),
		// Observed rather than required: the AAC cases ask for it themselves, since Chromium encodes AAC
		// only through a platform encoder, which Linux builds lack.
		"encode mp4a.40.2": await supports(() =>
			AudioEncoder.isConfigSupported({ codec: "mp4a.40.2", sampleRate: wanted.sampleRate, numberOfChannels: 1 }),
		),
		"decode mp4a.40.2": await supports(() =>
			AudioDecoder.isConfigSupported({ codec: "mp4a.40.2", sampleRate: wanted.sampleRate, numberOfChannels: 1 }),
		),
	};
}

/** Name missing APIs for a role; players can use WebSocket without an encoder or WebTransport. */
export function missing(found: Capabilities, role: Role): string[] {
	const required =
		role === "publisher"
			? [
					"WebTransport",
					"VideoEncoder",
					"AudioEncoder",
					"AudioWorkletNode",
					"MediaStreamTrackProcessor",
					"canvas.captureStream",
					"encode avc1.42001f",
					"encode vp8",
					"encode opus",
				]
			: [
					"WebSocket",
					"VideoDecoder",
					"AudioDecoder",
					"AudioWorkletNode",
					"decode avc1.42001f",
					"decode vp8",
					"decode opus",
				];
	return required.filter((name) => found[name] !== true);
}
