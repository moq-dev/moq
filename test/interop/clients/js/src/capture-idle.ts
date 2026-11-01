/** Real camera capture with independently attachable readers, including the element fallback. */
import * as Publish from "@moq/publish";
import { Effect } from "@moq/signals";

declare global {
	interface Window {
		captureIdle: Awaited<ReturnType<typeof open>>;
	}
}

async function open() {
	const fallback = new URLSearchParams(location.search).has("element");
	if (fallback) {
		Reflect.deleteProperty(window, "MediaStreamTrackProcessor");
		// Explicitly exercise the element path, even on engines with worker capture.
		Object.defineProperty(window, "Worker", {
			value: class {
				constructor() {
					throw new Error("element control");
				}
			},
		});
	}
	let processed = 0;
	const Frame = window.VideoFrame;
	window.VideoFrame = new Proxy(Frame, {
		construct(target, args) {
			processed++;
			return Reflect.construct(target, args);
		},
	});
	const media = await navigator.mediaDevices.getUserMedia({
		video: { width: 1280, height: 720, frameRate: 30 },
	});
	const track = media.getVideoTracks()[0];
	const capture = new Publish.Video.Capture({ source: track as Publish.Video.StreamTrack });
	const readers = new Map<number, Effect>();
	let delivered = 0;
	let last = -Infinity;
	let failure: string | undefined;
	return {
		state: () => ({
			processed,
			delivered,
			last,
			failure,
			device: track.readyState,
			display: capture.out.display.peek(),
		}),
		attach(id: number) {
			if (readers.has(id)) throw new Error("reader already attached");
			const fanout = capture.out.frames.peek();
			if (!fanout) throw new Error("capture not ready");
			const effect = new Effect();
			readers.set(id, effect);
			const reader = fanout.subscribe(effect).getReader();
			effect.spawn(async () => {
				let previous = -Infinity;
				try {
					for (;;) {
						const { value } = await reader.read();
						if (!value) return;
						const timestamp = value.timestamp;
						value.close();
						if (timestamp < previous) throw new Error("capture timestamp went backwards");
						previous = timestamp;
						delivered++;
						last = timestamp;
					}
				} catch (error) {
					failure = String(error);
				}
			});
		},
		detach(id: number) {
			readers.get(id)?.close();
			readers.delete(id);
		},
		close() {
			for (const effect of readers.values()) effect.close();
			capture.close();
			track.stop();
		},
	};
}

window.captureIdle = await open();
