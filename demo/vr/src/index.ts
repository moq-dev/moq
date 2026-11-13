import { Audio, Broadcast, Net, Signals, Sync, Video } from "@moq/watch";
import { type Placement, Spatial } from "./audio";
import { Scene } from "./scene";

const params = new URLSearchParams(location.search);
const url = new URL(params.get("url") ?? import.meta.env.VITE_RELAY_URL ?? "http://localhost:4443");
const name = Net.Path.from(params.get("name") ?? "vr.hang");

const connection = new Net.Connection({ url });
const broadcast = new Broadcast({ origin: connection.origin, name, enabled: true });
const audioSrc = new Audio.Source({ broadcast, supported: Audio.Decoder.supported });
const videoSrc = new Video.Source({ broadcast, supported: Video.Decoder.supported });
const sync = new Sync({});
const audio = new Audio.Decoder({ source: audioSrc, sync, enabled: true });
const video = new Video.Decoder({ source: videoSrc, sync, enabled: true });
const scene = new Scene(document.body);
const effect = new Signals.Effect();

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

effect.run((e) => {
	const config = e.get(videoSrc.out.config);
	if (config) scene.layout(config.projection, config.stereo);
});
effect.run((e) => {
	const root = e.get(audio.out.root);
	const config = e.get(audioSrc.out.config);
	if (!root || !config) return;
	const projection = e.get(videoSrc.out.config)?.projection?.kind;
	const placement: Placement =
		config.layout === "ambisonic"
			? { kind: "ambisonic" }
			: projection === "equirectangular"
				? { kind: "head" }
				: { kind: "point", position: scene.speaker };
	const spatial = new Spatial(root, placement);
	scene.listener = spatial;
	e.run((inner) => {
		spatial.muted = inner.get(scene.muted);
	});

	e.cleanup(() => {
		spatial.close();
		scene.listener = undefined;
	});
});
