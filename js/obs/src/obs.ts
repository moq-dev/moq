import { Duration, now, sleep, timeout } from "@moq/clock";
import { Effect, type GetPromise, type Getter, race, Signal } from "@moq/signals";
import * as z from "zod/mini";
import { Session } from "./session.ts";

/** obs-websocket's default port. */
export const PORT = 4455;

// The OBS release cpp/obs/buildspec.json builds the plugin against; `just obs check` keeps them equal.
const PLUGIN_OBS = "32.2.2";

// The profile `configure` uses unless told otherwise.
const PROFILE = "MoQ";

// How often `stats` refreshes while live: obs-websocket emits no stats event.
const POLL = Duration.secs(1);

// A profile switch runs on OBS's UI thread after the request returns; it takes milliseconds.
const SWITCH_TIMEOUT = Duration.secs(10);

// A profile parameter `configure` sets while a reload is owed, so a retry finishes it.
const PENDING = ["MoQ", "PendingReload"] as const;

// How long `start` waits to hear the stream went live or stopped. A start that fails in OBS
// before connecting shows an error dialog and emits neither, so this is the only sign of it.
const START_TIMEOUT = Duration.secs(30);

/** How to reach OBS. */
export interface ConnectProps {
	/** The password set in OBS under Tools > WebSocket Server Settings. */
	password?: string;
	/** The obs-websocket port on this machine. Defaults to {@link PORT}. */
	port?: number;
}

/** What OBS reports about itself. */
export interface Version {
	/** The OBS Studio version, such as `32.2.2`. */
	obs: string;
	/** The obs-websocket version. */
	websocket: string;
}

/** Where `configure` points OBS's stream. */
export type Destination =
	| {
			kind: "moq";
			/** The relay URL, such as `https://relay.example.com/anon`. */
			url: string | URL;
			/** The broadcast path to publish under. */
			broadcast: string;
			/** A token presented to the relay, kept out of the URL so a reconnect presents the current one. */
			token?: string;
			/** Video renditions to publish, each at `i/N` of the output size and bitrate. Defaults to 1. */
			layers?: number;
	  }
	| {
			kind: "rtmp";
			/** The RTMP(S) server URL, such as `rtmp://live.example.com/app`. */
			server: string | URL;
			/** The stream key. */
			key: string;
	  };

/** What `configure` writes to the profile. Anything left out keeps the profile's value. */
export interface Publish {
	/** The OBS profile to hold this destination, created when missing. Defaults to `MoQ`. */
	profile?: string;
	destination: Destination;
	video?: {
		/** The size OBS scales to before encoding; the canvas the scenes are laid out on stays. */
		size?: { width: number; height: number };
		/** Whole frames per second. */
		fps?: number;
		/** The top rendition's bitrate in kbps. */
		bitrate?: number;
	};
	audio?: {
		/** The audio bitrate in kbps. */
		bitrate?: number;
	};
}

/** A low-latency setting the profile's encoder cannot take from a profile. */
export type Ignored = "bframes" | "keyint" | "lookahead";

/** What `configure` applied. */
export interface Configured {
	/** The OBS profile now active. */
	profile: string;
	/** The profile's Simple output encoder, such as `x264` or `nvenc`. */
	encoder: string;
	/** Low-latency settings this encoder keeps at its own defaults. */
	ignored: Ignored[];
}

/** The stream output's state. */
export type StreamState = "stopped" | "starting" | "live" | "reconnecting" | "stopping";

/** The stream output's counters while it runs. */
export interface Stats {
	/** Bytes the output has sent since it last (re)started. */
	bytes: number;
	/** Bits per second over the last second; `undefined` until two samples exist. */
	bitrate: number | undefined;
	/** How congested the output is, from 0 to 1, as OBS's status bar shows it. */
	congestion: number;
	frames: {
		/** Frames the stream output handled. */
		total: number;
		/** Frames the stream output dropped because the network could not keep up. */
		dropped: number;
		/** Frames OBS skipped since going live because the encoder could not keep up. */
		skipped: number;
	};
}

/** A MoQ Source to add to a scene. */
export interface Source {
	/** The relay URL. */
	url: string | URL;
	/** The broadcast path to play. */
	broadcast: string;
	/** The source's name in OBS. Defaults to `MoQ <broadcast>`. */
	name?: string;
	/** The scene to add it to. Defaults to the scene on program. */
	scene?: string;
}

const VersionResponse = z.object({ obsVersion: z.string(), obsWebSocketVersion: z.string() });
const InputKinds = z.object({ inputKinds: z.array(z.string()) });
const Profiles = z.object({ currentProfileName: z.string(), profiles: z.array(z.string()) });
const Parameter = z.object({ parameterValue: z.nullable(z.string()) });
const VideoSettings = z.object({
	baseWidth: z.number(),
	baseHeight: z.number(),
	outputWidth: z.number(),
	outputHeight: z.number(),
	fpsNumerator: z.number(),
	fpsDenominator: z.number(),
});
const Outputs = z.object({ outputs: z.array(z.object({ outputName: z.string(), outputActive: z.boolean() })) });
const StreamStatus = z.object({
	outputActive: z.boolean(),
	outputReconnecting: z.boolean(),
	outputCongestion: z.number(),
	outputBytes: z.number(),
	outputSkippedFrames: z.number(),
	outputTotalFrames: z.number(),
});
const ObsStats = z.object({ outputSkippedFrames: z.number() });
const ProgramScene = z.object({ currentProgramSceneName: z.string() });
const StreamStateChanged = z.object({ outputActive: z.boolean(), outputState: z.string() });
const ProfileChanged = z.object({ profileName: z.string() });

const OUTPUT_STATES: Record<string, StreamState> = {
	OBS_WEBSOCKET_OUTPUT_STARTING: "starting",
	OBS_WEBSOCKET_OUTPUT_STARTED: "live",
	OBS_WEBSOCKET_OUTPUT_RECONNECTING: "reconnecting",
	OBS_WEBSOCKET_OUTPUT_RECONNECTED: "live",
	OBS_WEBSOCKET_OUTPUT_STOPPING: "stopping",
	OBS_WEBSOCKET_OUTPUT_STOPPED: "stopped",
};

/**
 * Connects to the OBS on this machine over its built-in WebSocket server.
 *
 * Throws a {@link ConnectError} whose `reason` says whether the browser was denied, nothing
 * answered, or OBS refused the password. The password stays in memory only.
 */
export async function connect(props: ConnectProps = {}): Promise<Obs> {
	const port = props.port ?? PORT;
	if (!Number.isInteger(port) || port < 1 || port > 65535) throw new RangeError(`invalid port: ${port}`);

	const session = await Session.connect(new URL(`ws://127.0.0.1:${port}`), props.password);
	try {
		const version = VersionResponse.parse(await session.request("GetVersion"));
		const kinds = InputKinds.parse(await session.request("GetInputKindList", { unversioned: true }));
		const status = StreamStatus.parse(await session.request("GetStreamStatus"));
		return Obs.make(
			session,
			{ obs: version.obsVersion, websocket: version.obsWebSocketVersion },
			kinds.inputKinds.includes("moq_source"),
			status.outputActive ? (status.outputReconnecting ? "reconnecting" : "live") : "stopped",
		);
	} catch (error) {
		session.close();
		throw error;
	}
}

/** A connection to the OBS on this machine. */
export class Obs {
	#session: Session;
	#effect = new Effect();
	#stream: Signal<StreamState>;
	#stats = new Signal<Stats | undefined>(undefined);

	/** The OBS and obs-websocket versions. */
	readonly version: Version;
	/** Whether OBS loaded the MoQ plugin, which `moq` destinations and {@link addSource} need. */
	readonly plugin: boolean;
	/** The stream output's state, following OBS as it changes. */
	readonly stream: Getter<StreamState>;
	/** The stream's counters, refreshed every second while it runs, `undefined` otherwise. */
	readonly stats: Getter<Stats | undefined>;
	/** Settles with `null` after {@link close}, or the error that ended the connection, such as OBS quitting. */
	readonly closed: GetPromise<Error | null>;

	/** Wrap a connected session. @internal Use {@link connect}. */
	static make(session: Session, version: Version, plugin: boolean, stream: StreamState): Obs {
		return new Obs(session, version, plugin, stream);
	}

	private constructor(session: Session, version: Version, plugin: boolean, stream: StreamState) {
		this.#session = session;
		this.version = version;
		this.plugin = plugin;
		this.#stream = new Signal(stream);
		this.stream = this.#stream;
		this.stats = this.#stats;
		this.closed = session.closed;

		this.#effect.cleanup(
			session.on("StreamStateChanged", (data) => {
				const event = StreamStateChanged.safeParse(data);
				if (!event.success) return;
				const { outputState, outputActive } = event.data;
				this.#stream.set(OUTPUT_STATES[outputState] ?? (outputActive ? "live" : "stopped"));
			}),
		);
		this.#effect.run((effect) => this.#poll(effect));
		void session.closed.then(() => this.#effect.close());
	}

	/**
	 * Puts `publish.destination` in a dedicated profile and tunes it for low latency, switching
	 * OBS to that profile. The user's other profiles keep their service and settings.
	 *
	 * Refuses while any OBS output runs, since switching profiles resets them.
	 */
	async configure(publish: Publish): Promise<Configured> {
		const destination = publish.destination;
		const layers = destination.kind === "moq" ? (destination.layers ?? 1) : 1;
		if (!Number.isInteger(layers) || layers < 1) throw new RangeError(`invalid layer count: ${layers}`);
		if (destination.kind === "moq") this.#requirePlugin();
		const fps = publish.video?.fps;
		if (fps !== undefined && (!Number.isInteger(fps) || fps < 1)) throw new RangeError(`invalid fps: ${fps}`);
		for (const kbps of [publish.video?.bitrate, publish.audio?.bitrate]) {
			if (kbps !== undefined && (!Number.isInteger(kbps) || kbps < 1))
				throw new RangeError(`invalid bitrate: ${kbps}`);
		}
		const size = publish.video?.size;
		if (size && !(fits(size.width) && fits(size.height))) {
			throw new RangeError(
				`invalid size: ${size.width}x${size.height}, each side must be ${MIN_SIDE} to ${MAX_SIDE}`,
			);
		}

		const active = Outputs.parse(await this.#session.request("GetOutputList")).outputs.filter(
			(o) => o.outputActive,
		);
		if (active.length > 0) {
			throw new Error(`stop OBS's running outputs first: ${active.map((o) => o.outputName).join(", ")}`);
		}

		const profile = publish.profile ?? PROFILE;
		const before = Profiles.parse(await this.#session.request("GetProfileList"));
		const created = !before.profiles.includes(profile);
		// A new profile's canvas is the monitor's, but the scene collection was laid out on this one.
		const inherited = created ? VideoSettings.parse(await this.#session.request("GetVideoSettings")) : undefined;
		if (inherited) {
			const sides = [inherited.baseWidth, inherited.baseHeight];
			if (!size) sides.push(inherited.outputWidth, inherited.outputHeight);
			if (!sides.every(fits)) {
				throw new Error(
					`OBS's video is larger than obs-websocket can copy into a new profile (${MAX_SIDE} per side); create "${profile}" in OBS's Profile menu`,
				);
			}
		}
		if (created) {
			await this.#activate(profile, () => this.#session.request("CreateProfile", { profileName: profile }));
		} else if (before.currentProfileName !== profile) {
			await this.#activate(profile, () => this.#session.request("SetCurrentProfile", { profileName: profile }));
		}

		// SetVideoSettings resets OBS's video, which leaves the simulcast layers on the old video
		// until the outputs are rebuilt: OBS 32.2.2 then sends no frames and crashes on stop.
		// So change it only when it differs, and reload the profile after when layers exist.
		const current = VideoSettings.parse(await this.#session.request("GetVideoSettings"));
		const base = inherited ?? current;
		const video: z.infer<typeof VideoSettings> = {
			baseWidth: base.baseWidth,
			baseHeight: base.baseHeight,
			outputWidth: size?.width ?? base.outputWidth,
			outputHeight: size?.height ?? base.outputHeight,
			fpsNumerator: fps ?? base.fpsNumerator,
			fpsDenominator: fps === undefined ? base.fpsDenominator : 1,
		};
		// obs-websocket sets each pair only when given, and checks the given ones against its limits.
		const pairs = [
			["baseWidth", "baseHeight"],
			["outputWidth", "outputHeight"],
			["fpsNumerator", "fpsDenominator"],
		] as const;
		const change = Object.fromEntries(
			pairs
				.filter((pair) => pair.some((key) => video[key] !== current[key]))
				.flatMap((pair) => pair.map((key) => [key, video[key]])),
		);
		const changed = Object.keys(change).length > 0;

		// OBS reads these when it builds its outputs, which only a profile load does.
		const reload: [string, string, string][] = [
			["Output", "Mode", "Simple"],
			["Stream1", "WHIPSimulcastTotalLayers", String(layers)],
			// Enhanced Broadcasting would replace an RTMP destination's encoders with its own.
			["Stream1", "EnableMultitrackVideo", "false"],
		];
		const stale = [];
		for (const [category, name, value] of reload) {
			if ((await this.#parameter(category, name)) !== value) stale.push(`${category}.${name}`);
		}
		if (changed && layers > 1) stale.push("Video");
		// Set until a reload finishes, so a retry after a dropped connection still reloads.
		if ((await this.#parameter(...PENDING)) !== null) stale.push("an interrupted reload");

		const other =
			before.currentProfileName !== profile
				? before.currentProfileName
				: before.profiles.find((p) => p !== profile);
		if (stale.length > 0 && other === undefined) {
			throw new Error(
				`applying ${stale.join(", ")} needs another profile to reload "${profile}" through: add one in OBS's Profile menu`,
			);
		}

		// OBS reads these on every StartStream.
		const keyint = Math.round((2 * video.fpsNumerator) / video.fpsDenominator);
		const options = `tune=zerolatency bframes=0 rc-lookahead=0 sync-lookahead=0 sliced-threads=1 mbtree=0 keyint=${keyint}`;
		const live: [string, string, string][] = [
			// A stream delay holds every frame back by its length, and OBS applies it per start.
			["Output", "DelayEnable", "false"],
			["SimpleOutput", "UseAdvanced", "true"],
			["SimpleOutput", "x264Settings", options],
		];
		if (publish.video?.bitrate !== undefined)
			live.push(["SimpleOutput", "VBitrate", String(publish.video.bitrate)]);
		if (publish.audio?.bitrate !== undefined)
			live.push(["SimpleOutput", "ABitrate", String(publish.audio.bitrate)]);

		if (stale.length > 0) await this.#setParameter(...PENDING, "true");

		if (destination.kind === "moq") {
			await this.#session.request("SetStreamServiceSettings", {
				streamServiceType: "moq_service",
				// OBS merges into a service of the same type, so clear a token a previous run left.
				streamServiceSettings: {
					server: String(destination.url),
					key: destination.broadcast,
					bearer_token: destination.token ?? "",
				},
			});
		} else {
			await this.#session.request("SetStreamServiceSettings", {
				streamServiceType: "rtmp_custom",
				streamServiceSettings: { server: String(destination.server), key: destination.key, use_auth: false },
			});
		}
		if (changed) await this.#session.request("SetVideoSettings", change);
		for (const [category, name, value] of [...reload, ...live]) await this.#setParameter(category, name, value);

		if (stale.length > 0 && other !== undefined) {
			await this.#activate(other, () => this.#session.request("SetCurrentProfile", { profileName: other }));
			await this.#activate(profile, () => this.#session.request("SetCurrentProfile", { profileName: profile }));
			await this.#setParameter(...PENDING, null);
		}

		const encoder = (await this.#parameter("SimpleOutput", "StreamEncoder")) ?? "";
		const ignored: Ignored[] = encoder.startsWith("x264") ? [] : ["bframes", "keyint", "lookahead"];
		return { profile, encoder, ignored };
	}

	/** Starts streaming and resolves once OBS is live, or rejects if the output stops first. */
	async start(): Promise<void> {
		await this.#transition("StartStream", "STARTED");
	}

	/** Stops streaming and resolves once the output has stopped. */
	async stop(): Promise<void> {
		await this.#transition("StopStream", "STOPPED");
	}

	/** Adds a MoQ Source playing `source.broadcast` to a scene. */
	async addSource(source: Source): Promise<void> {
		this.#requirePlugin();
		const scene =
			source.scene ??
			ProgramScene.parse(await this.#session.request("GetCurrentProgramScene")).currentProgramSceneName;
		await this.#session.request("CreateInput", {
			sceneName: scene,
			inputName: source.name ?? `MoQ ${source.broadcast}`,
			inputKind: "moq_source",
			inputSettings: { url: String(source.url), broadcast: source.broadcast },
			sceneItemEnabled: true,
		});
	}

	/** Closes the connection; OBS keeps running and keeps streaming if it was. */
	close(): void {
		this.#effect.close();
		this.#session.close();
	}

	#requirePlugin() {
		if (this.plugin) return;
		const older = compareVersions(this.version.obs, PLUGIN_OBS) < 0;
		throw new Error(
			older
				? `the MoQ plugin is not loaded: it needs OBS ${PLUGIN_OBS} or newer, and this is OBS ${this.version.obs}`
				: "the MoQ plugin is not loaded: install it from https://github.com/moq-dev/moq/releases?q=obs-moq",
		);
	}

	async #setParameter(parameterCategory: string, parameterName: string, parameterValue: string | null) {
		await this.#session.request("SetProfileParameter", { parameterCategory, parameterName, parameterValue });
	}

	async #parameter(parameterCategory: string, parameterName: string): Promise<string | null> {
		const response = await this.#session.request("GetProfileParameter", { parameterCategory, parameterName });
		return Parameter.parse(response).parameterValue;
	}

	// Runs `request` and waits for OBS to finish switching to `profile`, which it does after replying.
	async #activate(profile: string, request: () => Promise<unknown>) {
		const { promise, resolve } = Promise.withResolvers<void>();
		const dispose = this.#session.on("CurrentProfileChanged", (data) => {
			if (ProfileChanged.safeParse(data).data?.profileName === profile) resolve();
		});
		try {
			await request();
			await timeout(SWITCH_TIMEOUT, promise);
		} finally {
			dispose();
		}
	}

	// Resolves on the output state event `target`; follows the events rather than `stream`, which
	// coalesces a quick STARTING then STOPPED back into the value it started at.
	async #transition(request: string, target: "STARTED" | "STOPPED") {
		const { promise, resolve, reject } = Promise.withResolvers<void>();
		const dispose = this.#session.on("StreamStateChanged", (data) => {
			const state = StreamStateChanged.safeParse(data).data?.outputState;
			if (state === `OBS_WEBSOCKET_OUTPUT_${target}`) resolve();
			else if (state === "OBS_WEBSOCKET_OUTPUT_STOPPED") {
				reject(new Error("OBS stopped the stream before it went live; see the OBS log"));
			}
		});
		const abort = new AbortController();
		try {
			await this.#session.request(request);
			const waits: PromiseLike<true | "timeout" | Error | null>[] = [
				promise.then(() => true as const),
				this.closed,
			];
			if (target === "STARTED") {
				waits.push(sleep(START_TIMEOUT, { signal: abort.signal }).then(() => "timeout" as const));
			}
			let done = await race(waits);
			if (done === "timeout") {
				await this.#startTimedOut();
				done = await race([waits[0], this.closed]);
			}
			if (done !== true) throw done ?? new Error("the OBS connection is closed");
		} finally {
			abort.abort();
			dispose();
		}
	}

	// OBS sends no event when a start fails before connecting, so ask whether the output runs.
	// One that does is capturing ahead of its connection, as a stream delay does, and the
	// connection's own event settles it.
	async #startTimedOut() {
		const status = StreamStatus.parse(await this.#session.request("GetStreamStatus"));
		if (status.outputActive) return;
		// A start still connecting corrects this with its own event.
		this.#stream.set("stopped");
		throw new Error(`OBS did not go live within ${START_TIMEOUT.asSecs()} s; see OBS for the error`);
	}

	#poll(effect: Effect) {
		const state = effect.get(this.#stream);
		if (state !== "live" && state !== "reconnecting") {
			this.#stats.set(undefined);
			return;
		}

		const abort = effect.abort;
		effect.spawn(async () => {
			let skippedBase: number | undefined;
			let previous: { bytes: number; at: ReturnType<typeof now> } | undefined;
			while (!abort.aborted) {
				const [status, obs] = await Promise.all([
					this.#session.request("GetStreamStatus").then((data) => StreamStatus.parse(data)),
					this.#session.request("GetStats").then((data) => ObsStats.parse(data)),
				]).catch(() => [undefined, undefined] as const);
				if (!status || !obs || abort.aborted) return;

				const at = now();
				skippedBase ??= obs.outputSkippedFrames;
				// The byte count restarts when OBS reconnects the output.
				const sent =
					previous && status.outputBytes >= previous.bytes ? status.outputBytes - previous.bytes : undefined;
				const seconds = previous ? at.since(previous.at).asSecs() : 0;
				this.#stats.set({
					bytes: status.outputBytes,
					bitrate: sent !== undefined && seconds > 0 ? (sent * 8) / seconds : undefined,
					congestion: status.outputCongestion,
					frames: {
						total: status.outputTotalFrames,
						dropped: status.outputSkippedFrames,
						skipped: obs.outputSkippedFrames - skippedBase,
					},
				});
				previous = { bytes: status.outputBytes, at };
				await sleep(POLL, { signal: abort }).catch(() => {});
			}
		});
	}
}

// obs-websocket's bounds on each side of a canvas or output size.
const MIN_SIDE = 8;
const MAX_SIDE = 4096;

function fits(side: number): boolean {
	return Number.isInteger(side) && side >= MIN_SIDE && side <= MAX_SIDE;
}

// Compares dotted versions numerically; a missing part counts as 0.
function compareVersions(a: string, b: string): number {
	const pa = a.split(".").map((part) => Number.parseInt(part, 10) || 0);
	const pb = b.split(".").map((part) => Number.parseInt(part, 10) || 0);
	for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
		const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
		if (diff !== 0) return diff;
	}
	return 0;
}
