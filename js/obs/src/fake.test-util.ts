// An in-memory obs-websocket 5 server, installed as the global WebSocket, that models the OBS
// behavior @moq/obs depends on: profile switches finish after the reply, video resets need a
// profile reload for simulcast layers, and stream state arrives as events.

type Json = Record<string, unknown>;

export interface FakeOptions {
	password?: string;
	obsVersion?: string;
	plugin?: boolean;
	encoder?: string;
	// How the socket fails before it opens: nothing listening, or the constructor refusing.
	refuse?: "unreachable" | "throw";
	// Open the socket but never send Hello, like a server that is not obs-websocket.
	silent?: boolean;
	// StartStream goes STARTING then STOPPED, like a destination that refuses the connection.
	failStart?: boolean;
}

/** One request the fake received, in order. */
export type Logged = { type: string; data?: Json };

export class FakeObs {
	options: FakeOptions;
	log: Logged[] = [];
	profiles = new Map<string, Map<string, string>>([["Untitled", new Map()]]);
	current = "Untitled";
	service: { type: string; settings: Json } = { type: "rtmp_common", settings: {} };
	video = {
		fpsNumerator: 30,
		fpsDenominator: 1,
		baseWidth: 1920,
		baseHeight: 1080,
		outputWidth: 1920,
		outputHeight: 1080,
	};
	// Whether the simulcast layers point at the video OBS renders now; a video reset breaks them.
	layersFresh = true;
	streaming = false;
	bytes = 0;
	frames = 0;
	skipped = 0;
	outputs = [{ outputName: "simple_stream", outputActive: false }];
	sockets: FakeSocket[] = [];

	constructor(options: FakeOptions = {}) {
		this.options = options;
	}

	install(): Disposable {
		const original = globalThis.WebSocket;
		const fake = this;
		const Socket = class extends FakeSocket {
			constructor(url: string | URL, protocol?: string | string[]) {
				if (fake.options.refuse === "throw") throw new DOMException("insecure", "SecurityError");
				super(fake, String(url), protocol);
			}
		};
		globalThis.WebSocket = Socket as unknown as typeof WebSocket;
		return { [Symbol.dispose]: () => (globalThis.WebSocket = original) };
	}

	param(category: string, name: string): string | null {
		return this.profiles.get(this.current)?.get(`${category}.${name}`) ?? DEFAULTS[`${category}.${name}`] ?? null;
	}

	/** Pushes an event to every identified socket. */
	emit(eventType: string, eventData: Json) {
		for (const socket of this.sockets) socket.event(eventType, eventData);
	}

	/** Closes every socket from the server side, like OBS quitting. */
	quit() {
		for (const socket of this.sockets) socket.serverClose(1001, "OBS is exiting");
	}

	activate(name: string) {
		this.emit("CurrentProfileChanging", { profileName: this.current });
		this.current = name;
		// A profile load rebuilds the outputs, so the layers follow the current video again.
		this.layersFresh = true;
		this.emit("CurrentProfileChanged", { profileName: name });
	}

	stream(state: string, active: boolean) {
		this.streaming = active;
		this.outputs[0].outputActive = active;
		this.emit("StreamStateChanged", { outputActive: active, outputState: `OBS_WEBSOCKET_OUTPUT_${state}` });
	}

	// Returns response data, or throws [code, comment] for a failed status.
	handle(type: string, data: Json = {}): Json | undefined {
		this.log.push({ type, data });
		switch (type) {
			case "GetVersion":
				return { obsVersion: this.options.obsVersion ?? "32.2.2", obsWebSocketVersion: "5.7.4", rpcVersion: 1 };
			case "GetInputKindList":
				return { inputKinds: ["ffmpeg_source", ...(this.options.plugin === false ? [] : ["moq_source"])] };
			case "GetStreamStatus":
				return {
					outputActive: this.streaming,
					outputReconnecting: false,
					outputCongestion: 0.25,
					outputBytes: this.bytes,
					outputSkippedFrames: 2,
					outputTotalFrames: this.frames,
					outputDuration: 0,
					outputTimecode: "00:00:00.000",
				};
			case "GetStats":
				return { outputSkippedFrames: this.skipped, outputTotalFrames: 100, cpuUsage: 1 };
			case "GetOutputList":
				return { outputs: this.outputs };
			case "GetProfileList":
				return { currentProfileName: this.current, profiles: [...this.profiles.keys()] };
			case "CreateProfile": {
				const name = String(data.profileName);
				if (this.profiles.has(name)) throw [601, "exists"];
				this.profiles.set(name, new Map());
				// OBS queues the switch to its UI thread and replies first.
				queueMicrotask(() => this.activate(name));
				return undefined;
			}
			case "SetCurrentProfile": {
				const name = String(data.profileName);
				if (!this.profiles.has(name)) throw [600, "not found"];
				if (name !== this.current) this.activate(name);
				return undefined;
			}
			case "GetProfileParameter":
				return { parameterValue: this.param(String(data.parameterCategory), String(data.parameterName)) };
			case "SetProfileParameter":
				this.profiles
					.get(this.current)
					?.set(`${data.parameterCategory}.${data.parameterName}`, String(data.parameterValue));
				return undefined;
			case "GetVideoSettings":
				return { ...this.video };
			case "SetVideoSettings":
				if (this.streaming) throw [500, "output running"];
				Object.assign(this.video, data);
				this.layersFresh = false;
				return undefined;
			case "SetStreamServiceSettings": {
				if (this.streaming) throw [500, "output running"];
				const type = String(data.streamServiceType);
				const settings = data.streamServiceSettings as Json;
				// OBS merges into a service of the same type.
				this.service = {
					type,
					settings: type === this.service.type ? { ...this.service.settings, ...settings } : settings,
				};
				return undefined;
			}
			case "StartStream":
				if (this.streaming) throw [500, "output running"];
				queueMicrotask(() => {
					this.stream("STARTING", false);
					if (this.options.failStart) this.stream("STOPPED", false);
					else this.stream("STARTED", true);
				});
				return undefined;
			case "StopStream":
				if (!this.streaming) throw [501, "output not running"];
				queueMicrotask(() => {
					this.stream("STOPPING", true);
					this.stream("STOPPED", false);
				});
				return undefined;
			case "GetCurrentProgramScene":
				return { currentProgramSceneName: "Scene", sceneName: "Scene" };
			case "CreateInput":
				return { inputUuid: "uuid", sceneItemId: 1 };
			default:
				throw [204, `unknown request ${type}`];
		}
	}
}

const DEFAULTS: Record<string, string> = {
	"Output.Mode": "Simple",
	"SimpleOutput.StreamEncoder": "x264",
	"Stream1.EnableMultitrackVideo": "false",
};

class FakeSocket extends EventTarget {
	readonly url: string;
	readyState = 0;
	#fake: FakeObs;
	#identified = false;
	#challenge = "challenge";
	#salt = "salt";

	constructor(fake: FakeObs, url: string, protocol?: string | string[]) {
		super();
		this.#fake = fake;
		this.url = new URL(url).href;
		if (protocol !== "obswebsocket.json") throw new Error(`unexpected subprotocol ${protocol}`);
		queueMicrotask(() => {
			if (fake.options.refuse === "unreachable") {
				this.readyState = 3;
				this.dispatchEvent(new Event("error"));
				this.dispatchEvent(new CloseEvent("close", { code: 1006 }));
				return;
			}
			this.readyState = 1;
			this.dispatchEvent(new Event("open"));
			if (fake.options.silent) return;
			const authentication =
				fake.options.password === undefined ? undefined : { challenge: this.#challenge, salt: this.#salt };
			this.#send(0, { obsWebSocketVersion: "5.7.4", rpcVersion: 1, authentication });
		});
	}

	send(raw: string) {
		const { op, d } = JSON.parse(raw) as { op: number; d: Json };
		void this.#receive(op, d);
	}

	close() {
		if (this.readyState >= 2) return;
		this.serverClose(1000, "");
	}

	serverClose(code: number, reason: string) {
		if (this.readyState >= 2) return;
		this.readyState = 3;
		this.#fake.sockets = this.#fake.sockets.filter((s) => s !== this);
		queueMicrotask(() => this.dispatchEvent(new CloseEvent("close", { code, reason })));
	}

	/** Sends a raw text frame to the client. */
	raw(data: string) {
		this.dispatchEvent(new MessageEvent("message", { data }));
	}

	event(eventType: string, eventData: Json) {
		if (this.#identified) this.#send(5, { eventType, eventIntent: 0, eventData });
	}

	async #receive(op: number, d: Json) {
		if (op === 1) {
			const password = this.#fake.options.password;
			if (
				password !== undefined &&
				d.authentication !== (await expected(password, this.#salt, this.#challenge))
			) {
				this.serverClose(4009, "Authentication failed.");
				return;
			}
			this.#identified = true;
			this.#fake.sockets.push(this);
			this.#send(2, { negotiatedRpcVersion: 1 });
		} else if (op === 6) {
			const { requestType, requestId, requestData } = d as {
				requestType: string;
				requestId: string;
				requestData?: Json;
			};
			try {
				const responseData = this.#fake.handle(requestType, requestData);
				this.#send(7, { requestType, requestId, requestStatus: { result: true, code: 100 }, responseData });
			} catch (error) {
				if (!Array.isArray(error)) throw error;
				const [code, comment] = error;
				this.#send(7, { requestType, requestId, requestStatus: { result: false, code, comment } });
			}
		}
	}

	#send(op: number, d: Json) {
		this.raw(JSON.stringify({ op, d }));
	}
}

async function expected(password: string, salt: string, challenge: string): Promise<string> {
	const hash = async (input: string) =>
		Buffer.from(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input))).toString("base64");
	return hash((await hash(password + salt)) + challenge);
}
