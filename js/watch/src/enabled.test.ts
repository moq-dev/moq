import { expect, mock, test } from "bun:test";

// Bun cannot load the blob-URL worklet import used by the audio decoder. This default-input test
// never creates an AudioContext or reaches the worklet implementation.
mock.module("./audio/render-worklet.ts?worklet", () => ({ default: "blob:fake-render" }));

const Watch = await import("./index");

type Mode = "omitted" | boolean | undefined;

function inputs(mode: Mode): { enabled?: boolean } | undefined {
	return mode === "omitted" ? undefined : { enabled: mode };
}

function enabled(mode: Mode): boolean[] {
	const sync = new Watch.Sync();
	const audioSource = new Watch.Audio.Source();
	const videoSource = new Watch.Video.Source();
	const textSource = new Watch.Text.Source();
	const props = inputs(mode);
	const components = [
		new Watch.Broadcast(props),
		new Watch.Audio.Decoder(audioSource, sync, props),
		new Watch.Video.Decoder(videoSource, sync, props),
		new Watch.Text.Renderer(textSource, sync, props),
	];

	try {
		return components.map((component) => component.in.enabled.peek());
	} finally {
		for (const component of components) component.close();
		audioSource.close();
		videoSource.close();
		textSource.close();
		sync.close();
	}
}

test("standalone components default enabled", () => {
	expect(enabled("omitted")).toEqual([true, true, true, true]);
	expect(enabled(undefined)).toEqual([true, true, true, true]);
	expect(enabled(false)).toEqual([false, false, false, false]);
});
