/** Native browser capabilities, kept separate from publisher and player requirements. @module */

/** Which side of the media fixture an engine runs. */
export type Role = "publisher" | "player";

/** Every native capability the media harness observes. */
export type Capabilities = Record<string, boolean>;

/**
 * How `@moq/publish` turns a camera track into frames on this engine, in the order it tries them
 * (see `js/publish/src/video/processor.ts`): a window `MediaStreamTrackProcessor`, one in a capture
 * worker the track is transferred to, or a `<video>` element sampled per presented frame.
 */
export type CapturePath = "window" | "worker" | "element";

/** The capture path production takes, given what {@link probe} found, or undefined when none works. */
export function capturePath(found: Capabilities): CapturePath | undefined {
	if (found.MediaStreamTrackProcessor) return "window";
	if (found["worker MediaStreamTrackProcessor"] && found["MediaStreamTrack transfer"]) return "worker";
	if (found.requestVideoFrameCallback) return "element";
	return undefined;
}

/** The name {@link missing} gives an engine without any {@link CapturePath}. */
export const NO_CAPTURE = "a capture path (MediaStreamTrackProcessor, in a worker, or requestVideoFrameCallback)";

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
	// Ask a worker what the capture worker would find: the API, and whether a transferred track
	// arrives. A blob: worker, as production's is without assets().
	const worker = async () => {
		const source = `onmessage = (e) => postMessage({ transferred: e.data instanceof MediaStreamTrack });
			postMessage({ supported: "MediaStreamTrackProcessor" in self });`;
		const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
		const spawned = new Worker(url);
		const next = () =>
			new Promise<Record<string, boolean>>((resolve, reject) => {
				spawned.onmessage = (event) => resolve(event.data);
				spawned.onerror = (event) => reject(new Error(event.message));
			});
		try {
			const { supported } = await next();
			const track = document.createElement("canvas").captureStream(0).getVideoTracks()[0];
			try {
				// The DOM lib does not list MediaStreamTrack as transferable.
				spawned.postMessage(track, [track as unknown as Transferable]);
				return { supported, transferred: (await next()).transferred === true };
			} catch {
				track.stop();
				return { supported, transferred: false };
			}
		} catch {
			return { supported: false, transferred: false };
		} finally {
			spawned.terminate();
			URL.revokeObjectURL(url);
		}
	};
	const inWorker = await worker();
	return {
		WebTransport: has("WebTransport"),
		WebSocket: has("WebSocket"),
		VideoDecoder: has("VideoDecoder"),
		VideoEncoder: has("VideoEncoder"),
		AudioDecoder: has("AudioDecoder"),
		AudioEncoder: has("AudioEncoder"),
		AudioWorkletNode: has("AudioWorkletNode"),
		MediaStreamTrackProcessor: has("MediaStreamTrackProcessor"),
		"worker MediaStreamTrackProcessor": inWorker.supported,
		"MediaStreamTrack transfer": inWorker.transferred,
		requestVideoFrameCallback: typeof HTMLVideoElement.prototype.requestVideoFrameCallback === "function",
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
	};
}

/**
 * Name missing APIs for a role. Either role can use WebSocket without WebTransport, and a publisher
 * needs any one capture path rather than a particular one.
 */
export function missing(found: Capabilities, role: Role): string[] {
	const required =
		role === "publisher"
			? [
					"WebSocket",
					"VideoEncoder",
					"AudioEncoder",
					"AudioWorkletNode",
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
	const absent = required.filter((name) => found[name] !== true);
	if (role === "publisher" && capturePath(found) === undefined) absent.push(NO_CAPTURE);
	return absent;
}
