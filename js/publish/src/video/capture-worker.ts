// Runs as a module Web Worker (started from ./polyfill.ts). Compiled and inlined as a blob URL by
// vite-plugin-worklet via the ?worklet suffix.
//
// Safari only exposes MediaStreamTrackProcessor inside a Worker. Capturing here rather than on the
// main thread also keeps frames flowing while the publish window is occluded, where the browser
// throttles the main-thread requestAnimationFrame loop to a freeze.

/** Messages the capture worker posts back to the host (./polyfill.ts). */
export type CaptureToHost =
	| { type: "frame"; frame: VideoFrame }
	| { type: "done" }
	| { type: "error"; message: string };

// `self` is the worker global scope. The project's DOM lib doesn't ship the worker-scope types, so
// declare just the slice we use instead of pulling in the conflicting webworker lib.
const scope = self as unknown as {
	onmessage: ((event: MessageEvent<{ track: MediaStreamTrack }>) => void) | null;
	postMessage(message: CaptureToHost, transfer?: Transferable[]): void;
};

scope.onmessage = async (event: MessageEvent<{ track: MediaStreamTrack }>) => {
	const { track } = event.data;

	try {
		// The host only starts this worker on a known Safari 18+, where the global exists. If it were
		// somehow missing, `new MediaStreamTrackProcessor` throws ReferenceError, which the catch below
		// reports to the host as an error rather than hanging with no frames.
		// @ts-expect-error MediaStreamTrackProcessor has no TypeScript types yet.
		const processor = new MediaStreamTrackProcessor({ track });
		const reader: ReadableStreamDefaultReader<VideoFrame> = processor.readable.getReader();

		for (;;) {
			const { value: frame, done } = await reader.read();
			if (done || !frame) break;

			// Transfer ownership to the host, which rewrites the timestamp and closes the frame.
			scope.postMessage({ type: "frame", frame }, [frame]);
		}

		scope.postMessage({ type: "done" });
	} catch (err) {
		scope.postMessage({ type: "error", message: String(err) });
	} finally {
		// Release the transferred camera clone. The host terminates this worker on any terminal
		// message; we deliberately do not self-close, so a terminal message can never race a
		// self.close() that might drop it before it reaches the host.
		try {
			track.stop();
		} catch {
			// Already stopped or neutered by transfer; nothing to do.
		}
	}
};
