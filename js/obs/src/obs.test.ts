import { afterEach, beforeEach, expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { FakeObs, type FakeOptions } from "./fake.test-util.ts";
import { ConnectError, connect, type Publish, RequestError } from "./index.ts";

let clock: Manual;
let installed: { close(): void };
let fake: FakeObs;
let restore: Disposable;

beforeEach(() => {
	clock = new Manual();
	installed = clock.install();
});

afterEach(() => {
	restore?.[Symbol.dispose]();
	installed.close();
});

function obs(options: FakeOptions = {}) {
	fake = new FakeObs(options);
	restore = fake.install();
	return fake;
}

async function reason(promise: Promise<unknown>): Promise<string> {
	const error = await promise.then(
		() => undefined,
		(error: unknown) => error,
	);
	if (!(error instanceof ConnectError)) throw new Error(`expected a ConnectError, got ${String(error)}`);
	return error.reason;
}

const MOQ: Publish = {
	destination: { kind: "moq", url: "https://relay.example/anon", broadcast: "me.hang", layers: 3 },
};

test("authenticates with the password", async () => {
	obs({ password: "secret" });
	const client = await connect({ password: "secret" });
	expect(client.version).toEqual({ obs: "32.2.2", websocket: "5.7.4" });
	expect(client.plugin).toBe(true);
	expect(client.stream.peek()).toBe("stopped");
	client.close();
	expect(await client.closed).toBeNull();
});

test("tells each connect failure apart", async () => {
	obs({ password: "secret" });
	expect(await reason(connect({ password: "wrong" }))).toBe("refused");
	expect(await reason(connect())).toBe("refused");

	restore[Symbol.dispose]();
	obs({ refuse: "unreachable" });
	expect(await reason(connect())).toBe("unreachable");

	restore[Symbol.dispose]();
	obs({ refuse: "throw" });
	expect(await reason(connect())).toBe("blocked");
});

test("reports a loopback prompt the user refused or left unanswered as denied", async () => {
	obs({ refuse: "unreachable" });
	const permissions = Object.getOwnPropertyDescriptor(globalThis.navigator, "permissions");
	const location = Object.getOwnPropertyDescriptor(globalThis, "location");
	let state: PermissionState = "denied";
	Object.defineProperty(globalThis.navigator, "permissions", {
		configurable: true,
		value: {
			query: async ({ name }: { name: string }) => {
				if (name !== "loopback-network") throw new TypeError("unknown permission");
				return { state };
			},
		},
	});
	Object.defineProperty(globalThis, "location", { configurable: true, value: new URL("https://site.example/") });
	try {
		expect(await reason(connect())).toBe("denied");
		state = "prompt";
		expect(await reason(connect())).toBe("denied");
		state = "granted";
		expect(await reason(connect())).toBe("unreachable");

		// A page served from loopback needs no permission, so a prompt state means nothing there.
		state = "prompt";
		Object.defineProperty(globalThis, "location", { configurable: true, value: new URL("http://localhost:5173/") });
		expect(await reason(connect())).toBe("unreachable");
	} finally {
		if (permissions) Object.defineProperty(globalThis.navigator, "permissions", permissions);
		else delete (globalThis.navigator as { permissions?: unknown }).permissions;
		if (location) Object.defineProperty(globalThis, "location", location);
		else delete (globalThis as { location?: unknown }).location;
	}
});

test("gives up on a server that never says Hello", async () => {
	obs({ silent: true });
	const pending = reason(connect());
	await clock.advance(Duration.secs(5));
	expect(await pending).toBe("unsupported");
});

test("rejects a port outside the valid range", async () => {
	obs();
	await expect(connect({ port: 0 })).rejects.toThrow(RangeError);
	await expect(connect({ port: 65536 })).rejects.toThrow(RangeError);
});

test("puts a MoQ destination in a new profile and reloads it for the layers", async () => {
	obs();
	const client = await connect();
	const configured = await client.configure({
		...MOQ,
		destination: { ...MOQ.destination, kind: "moq", token: "jwt" } as Publish["destination"],
		video: { size: { width: 1280, height: 720 }, fps: 60, bitrate: 3000 },
		audio: { bitrate: 128 },
	});

	expect(configured).toEqual({ profile: "MoQ", encoder: "x264", ignored: [] });
	expect(fake.current).toBe("MoQ");
	expect(fake.profiles.get("Untitled")?.size).toBe(0);
	expect(fake.service).toEqual({
		type: "moq_service",
		settings: { server: "https://relay.example/anon", key: "me.hang", bearer_token: "jwt" },
	});
	expect(fake.video).toMatchObject({ outputWidth: 1280, outputHeight: 720, fpsNumerator: 60, fpsDenominator: 1 });
	expect(fake.video).toMatchObject({ baseWidth: 1920, baseHeight: 1080 });
	expect(fake.param("Stream1", "WHIPSimulcastTotalLayers")).toBe("3");
	expect(fake.param("SimpleOutput", "VBitrate")).toBe("3000");
	expect(fake.param("SimpleOutput", "ABitrate")).toBe("128");
	expect(fake.param("SimpleOutput", "x264Settings")).toContain("bframes=0");
	expect(fake.param("SimpleOutput", "x264Settings")).toContain("keyint=120");

	// The reload went through the other profile, after every write.
	const types = fake.log.map((entry) => entry.type);
	const switches = fake.log
		.filter((entry) => entry.type === "SetCurrentProfile")
		.map((entry) => entry.data?.profileName);
	expect(switches).toEqual(["Untitled", "MoQ"]);
	expect(types.lastIndexOf("SetProfileParameter")).toBeLessThan(types.indexOf("SetCurrentProfile"));
	expect(fake.layersFresh).toBe(true);
	client.close();
});

test("leaves an up-to-date profile alone", async () => {
	obs();
	const client = await connect();
	const publish = { ...MOQ, video: { size: { width: 1280, height: 720 }, fps: 30 } };
	await client.configure(publish);
	fake.log = [];

	// A token change rewrites the service but needs no reload or video reset.
	await client.configure({
		...publish,
		destination: { ...MOQ.destination, kind: "moq", token: "new" } as Publish["destination"],
	});
	const types = fake.log.map((entry) => entry.type);
	expect(types).not.toContain("SetCurrentProfile");
	expect(types).not.toContain("SetVideoSettings");
	expect(types).not.toContain("CreateProfile");
	expect(fake.service.settings.bearer_token).toBe("new");

	// Dropping the token clears the one OBS would otherwise keep.
	await client.configure(publish);
	expect(fake.service.settings.bearer_token).toBe("");
	client.close();
});

test("a video change with layers reloads the profile so the layers follow it", async () => {
	obs();
	const client = await connect();
	await client.configure(MOQ);
	fake.log = [];

	await client.configure({ ...MOQ, video: { size: { width: 640, height: 360 } } });
	const types = fake.log.map((entry) => entry.type);
	expect(types.indexOf("SetVideoSettings")).toBeLessThan(types.indexOf("SetCurrentProfile"));
	expect(fake.layersFresh).toBe(true);

	// One layer has nothing to go stale, so it skips the reload.
	await client.configure({
		...MOQ,
		destination: { ...MOQ.destination, kind: "moq", layers: 1 } as Publish["destination"],
	});
	fake.log = [];
	await client.configure({
		...MOQ,
		destination: { ...MOQ.destination, kind: "moq", layers: 1 } as Publish["destination"],
		video: { fps: 60 },
	});
	expect(fake.log.map((entry) => entry.type)).not.toContain("SetCurrentProfile");
	client.close();
});

test("refuses to reload without another profile to go through", async () => {
	obs();
	fake.profiles = new Map([["MoQ", new Map()]]);
	fake.current = "MoQ";
	const client = await connect();
	await expect(client.configure(MOQ)).rejects.toThrow("restart OBS to apply Stream1.WHIPSimulcastTotalLayers");
	client.close();
});

test("puts an RTMP destination in the profile with one rendition", async () => {
	obs({ plugin: false });
	const client = await connect();
	await client.configure({ destination: { kind: "rtmp", server: "rtmps://live.example/app", key: "key" } });
	expect(fake.service).toEqual({
		type: "rtmp_custom",
		settings: { server: "rtmps://live.example/app", key: "key", use_auth: false },
	});
	expect(fake.param("Stream1", "WHIPSimulcastTotalLayers")).toBe("1");
	client.close();
});

test("names the low-latency settings an encoder keeps", async () => {
	obs();
	fake.profiles.set("MoQ", new Map([["SimpleOutput.StreamEncoder", "nvenc"]]));
	const client = await connect();
	const configured = await client.configure(MOQ);
	expect(configured).toEqual({ profile: "MoQ", encoder: "nvenc", ignored: ["bframes", "keyint", "lookahead"] });
	client.close();
});

test("refuses while an output runs", async () => {
	obs();
	fake.outputs.push({ outputName: "virtualcam_output", outputActive: true });
	const client = await connect();
	await expect(client.configure(MOQ)).rejects.toThrow("stop OBS's running outputs first: virtualcam_output");
	expect(fake.log.map((entry) => entry.type)).not.toContain("CreateProfile");
	client.close();
});

test("refuses MoQ without the plugin, naming the OBS it needs", async () => {
	obs({ plugin: false, obsVersion: "31.1.0" });
	const client = await connect();
	expect(client.plugin).toBe(false);
	await expect(client.configure(MOQ)).rejects.toThrow("it needs OBS 32.2.2 or newer, and this is OBS 31.1.0");
	await expect(client.addSource({ url: "https://relay.example", broadcast: "bbb.hang" })).rejects.toThrow(
		"not loaded",
	);
	client.close();

	restore[Symbol.dispose]();
	obs({ plugin: false, obsVersion: "32.3.0" });
	const current = await connect();
	await expect(current.configure(MOQ)).rejects.toThrow("install it");
	current.close();
});

test("rejects invalid settings before touching OBS", async () => {
	obs();
	const client = await connect();
	fake.log = [];
	const layers = (layers: number) => ({ destination: { ...MOQ.destination, layers } }) as Publish;
	await expect(client.configure(layers(0))).rejects.toThrow(RangeError);
	await expect(client.configure(layers(1.5))).rejects.toThrow(RangeError);
	await expect(client.configure({ ...MOQ, video: { fps: 29.97 } })).rejects.toThrow(RangeError);
	await expect(client.configure({ ...MOQ, audio: { bitrate: -1 } })).rejects.toThrow(RangeError);
	expect(fake.log).toEqual([]);
	client.close();
});

test("starts, follows the state, and stops", async () => {
	obs();
	const client = await connect();
	await client.start();
	expect(client.stream.peek()).toBe("live");
	await client.stop();
	expect(client.stream.peek()).toBe("stopped");
	await expect(client.stop()).rejects.toBeInstanceOf(RequestError);
	client.close();
});

test("a start the output abandons rejects", async () => {
	obs({ failStart: true });
	const client = await connect();
	await expect(client.start()).rejects.toThrow("OBS stopped the stream before it went live");
	client.close();
});

test("polls stats only while live", async () => {
	obs();
	const client = await connect();
	fake.bytes = 1000;
	fake.frames = 30;
	fake.skipped = 5;
	await client.start();
	await clock.advance(Duration.ZERO);
	expect(client.stats.peek()).toEqual({
		bytes: 1000,
		bitrate: undefined,
		congestion: 0.25,
		frames: { total: 30, dropped: 2, skipped: 0 },
	});

	fake.bytes = 2000;
	fake.skipped = 7;
	await clock.advance(Duration.secs(1));
	expect(client.stats.peek()).toMatchObject({ bytes: 2000, bitrate: 8000, frames: { skipped: 2 } });

	// The count restarts when OBS reconnects the output; that sample has no rate.
	fake.bytes = 100;
	await clock.advance(Duration.secs(1));
	expect(client.stats.peek()?.bitrate).toBeUndefined();

	await client.stop();
	await clock.advance(Duration.ZERO);
	expect(client.stats.peek()).toBeUndefined();
	const polls = fake.log.filter((entry) => entry.type === "GetStats").length;
	await clock.advance(Duration.secs(5));
	expect(fake.log.filter((entry) => entry.type === "GetStats").length).toBe(polls);
	client.close();
});

test("adds a MoQ Source to the program scene", async () => {
	obs();
	const client = await connect();
	await client.addSource({ url: "https://relay.example", broadcast: "bbb.hang" });
	await client.addSource({ url: new URL("https://relay.example"), broadcast: "x", name: "Guest", scene: "Other" });
	const created = fake.log.filter((entry) => entry.type === "CreateInput").map((entry) => entry.data);
	expect(created).toEqual([
		{
			sceneName: "Scene",
			inputName: "MoQ bbb.hang",
			inputKind: "moq_source",
			inputSettings: { url: "https://relay.example", broadcast: "bbb.hang" },
			sceneItemEnabled: true,
		},
		{
			sceneName: "Other",
			inputName: "Guest",
			inputKind: "moq_source",
			inputSettings: { url: "https://relay.example/", broadcast: "x" },
			sceneItemEnabled: true,
		},
	]);
	client.close();
});

test("closes with an error when OBS goes away", async () => {
	obs();
	const client = await connect();
	fake.quit();
	const error = await client.closed;
	expect(error?.message).toContain("OBS closed the connection (code 1001)");
	await expect(client.start()).rejects.toThrow("code 1001");
});

test("closes with an error on a malformed message", async () => {
	obs();
	const client = await connect();
	fake.sockets[0]?.raw("not json");
	expect((await client.closed)?.message).toBe("OBS sent a malformed message");
});
