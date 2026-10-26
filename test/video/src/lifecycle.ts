import type MoqPublish from "@moq/publish/element";
import type MoqWatch from "@moq/watch/element";

declare global {
	interface Window {
		lifecycle: ReturnType<typeof lifecycle>;
		startLifecycle(): void;
	}
}

function lifecycle() {
	const publisher = document.createElement("moq-publish") as MoqPublish;
	publisher.source = "camera";
	const preview = document.createElement("video");
	publisher.append(preview);
	const watcher = document.createElement("moq-watch") as MoqWatch;
	watcher.setAttribute("visible", "always");
	watcher.append(document.createElement("canvas"));
	document.body.append(publisher, watcher);
	let tracks: MediaStreamTrack[] = [];
	let context: AudioContext | undefined;
	return {
		state() {
			const stream = preview.srcObject as MediaStream | null;
			if (stream && tracks.length < 2) {
				const audio = publisher.audio.capture?.in.source.peek();
				tracks = [...stream.getTracks(), ...(audio && "track" in audio ? [audio.track] : [])];
			}
			context ??= publisher.audio.capture?.out.root.peek()?.context as AudioContext | undefined;
			return {
				tracks: tracks.map((track) => track.readyState),
				context: context?.state,
				preview: preview.srcObject !== null,
				devices: window.moqGpu.devices,
				sources: !!publisher.sources.video.peek() || !!publisher.sources.audio.peek(),
			};
		},
		move() {
			publisher.remove();
			watcher.remove();
			document.body.append(publisher, watcher);
		},
		detach() {
			publisher.remove();
			watcher.remove();
		},
		reconnect() {
			tracks = [];
			context = undefined;
			document.body.append(publisher, watcher);
		},
		close() {
			publisher.remove();
			watcher.remove();
			publisher.signals.close();
			watcher.signals.close();
		},
	};
}
window.startLifecycle = () => {
	window.lifecycle = lifecycle();
};
