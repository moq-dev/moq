// Runs as a module Web Worker (started from ./polyfill.ts). Compiled and inlined as a blob URL by
// vite-plugin-worklet via the ?worklet suffix.
//
// Safari only exposes MediaStreamTrackProcessor inside a Worker. Capturing here rather than on the
// main thread also keeps frames flowing while the publish window is occluded, where the browser
// throttles the main-thread requestAnimationFrame loop to a freeze.

// `self` is the worker global scope. The project's DOM lib doesn't ship the worker-scope types, so
// declare just the slice we use (a transfer-list postMessage and onmessage) instead of pulling in
// the conflicting webworker lib.
const scope = self as unknown as {
	onmessage: ((event: MessageEvent<{ track: MediaStreamTrack }>) => void) | null;
	postMessage(message: unknown, transfer?: unknown[]): void;
};

scope.onmessage = async (event: MessageEvent<{ track: MediaStreamTrack }>) => {
	const { track } = event.data;

	// @ts-expect-error MediaStreamTrackProcessor has no TypeScript types yet.
	const processor = new MediaStreamTrackProcessor({ track });
	const reader: ReadableStreamDefaultReader<VideoFrame> = processor.readable.getReader();

	try {
		for (;;) {
			const { value: frame, done } = await reader.read();
			if (done || !frame) break;

			// Transfer ownership to the main thread, which rewrites the timestamp and closes the frame.
			scope.postMessage({ frame }, [frame]);
		}
	} catch (err) {
		scope.postMessage({ error: String(err) });
	}
};
