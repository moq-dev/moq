import { Audio, Broadcast, Net, Signals, Sync, Video } from "@moq/watch";
import { type Placement, Spatial } from "./audio";
import { Scene } from "./scene";

const params = new URLSearchParams(location.search);
const url = new URL(params.get("url") ?? import.meta.env.VITE_RELAY_URL ?? "http://localhost:4443");
const name = Net.Path.from(params.get("name") ?? "vr.hang");

const connection = new Net.Connection({ url });
const broadcast = new Broadcast({ origin: connection.origin, name, enabled: true });
const audioSrc = new Audio.Source({ broadcast });
const videoSrc = new Video.Source({ broadcast });
const sync = new Sync({});
const audio = new Audio.Decoder({ source: audioSrc, sync, enabled: true });
const video = new Video.Decoder({ source: videoSrc, sync, enabled: true });
const scene = new Scene(document.body);
const effect = new Signals.Effect();

// The console and test/vr read the catalog and the scene through this.
Object.assign(window, { vr: { audio: audioSrc, video: videoSrc, scene } });

// clone the frame once decoder gets it out
effect.run((e) => {
	const frame = e.get(video.out.frame);
	if (frame) scene.frame(frame.clone());
});

effect.run((e) => {
	const error = e.get(video.out.error) ?? e.get(audio.out.error);
	const muted = e.get(scene.muted) ? " · muted" : "";
	const config = e.get(videoSrc.out.config);
	const display = e.get(video.out.display);
	const delay = e.get(sync.out.delay);
	const fps = e.get(scene.fps);
	// Stats change every frame, so sample them on the once-a-second fps tick instead.
	const stats = video.out.stats.peek();

	const projection = config?.projection;
	const shape = projection?.kind === "equirectangular" ? `equirect ${projection.fov ?? 360}°` : "flat";
	const lines = [
		error
			? `error: ${error.message}`
			: `${e.get(connection.status)} · ${name} ${e.get(broadcast.out.status)}${muted}`,
		config
			? `${display?.width ?? "?"}×${display?.height ?? "?"} ${config.codec} · ${shape} · ${config.stereo ?? "mono"}`
			: "no video",
		`render ${fps.render} fps · video ${fps.video} fps`,
		`buffer ${Math.round(delay.asMillis())} ms · late ${stats?.late ?? 0} · stalls ${stats?.stalls ?? 0}`,
	];
	scene.status(lines);
});

// The publisher republishes its catalog as its bitrate and timing estimates settle, so the picture
// and the sound rebuild only when a field they lay out by changes. Rebuilding the audio graph on
// every update would glitch the sound and drop anything tapping the mix.
const picture = effect.computed((e) => {
	const config = e.get(videoSrc.out.config);
	return config && { projection: config.projection, stereo: config.stereo };
});
const placement = effect.computed((e): Placement | undefined => {
	const config = e.get(audioSrc.out.config);
	if (!config) return undefined;
	if (config.layout === "ambisonic") return { kind: "ambisonic" };
	if (e.get(picture)?.projection?.kind === "equirectangular") return { kind: "head" };
	return { kind: "point", position: scene.speaker };
});

effect.run((e) => {
	const layout = e.get(picture);
	if (layout) scene.layout(layout.projection, layout.stereo);
});
effect.run((e) => {
	const root = e.get(audio.out.root);
	const where = e.get(placement);
	if (!root || !where) return;
	const spatial = new Spatial(root, where);
	scene.listener = spatial;
	e.run((inner) => {
		spatial.muted = inner.get(scene.muted);
	});

	e.cleanup(() => {
		spatial.close();
		scene.listener = undefined;
	});
});
