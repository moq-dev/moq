// The OfflineMeansOffline and RefusalIsExplained paths in `verify/Watch.tla`: the player reads
// "connecting" rather than "offline" until a session could have announced the broadcast, so the
// viewer is never told OFFLINE about its own connection, and a refused token says why.
import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Origin, Path } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { key } from "../../json/src/key.test-util.ts";
import { accept, connect } from "../../net/src/connection/index.ts";
import * as Lite from "../../net/src/lite/index.ts";
import { createMockTransportPair, waitFor } from "../../net/src/mock.ts";
import { Broadcast } from "./broadcast";
import type MoqWatch from "./element";
import { offlineIndicator } from "./ui/components/offline-indicator";
import { refusedIndicator } from "./ui/components/refused-indicator";

const url = new URL("https://relay.example/room");

test("a live broadcast reads connecting until connected and again once the connection drops", async () => {
	// The publisher is live on the relay until the end.
	const relay = new Origin.Producer();
	const name = Path.from("live.hang");
	const route = relay.route(name);
	const producer = new Json.Snapshot.Producer<Catalog.Root>({
		track: route.broadcast.createTrack(Catalog.TRACK),
		deltas: false,
	});
	producer.update({ value: {}, at: key() });

	const viewer = new Origin.Producer();
	const source = new Broadcast({ origin: viewer, name, catalogFormat: "hang", enabled: true });
	const seen = new Set<string>();
	const watching = new Effect();
	watching.run((effect) => {
		seen.add(effect.get(source.out.status));
	});
	const dial = async () => {
		const pair = createMockTransportPair(Lite.ALPN_06);
		return Promise.all([
			connect({ url, transport: pair.client, consume: viewer }),
			accept({ transport: pair.server, url, publish: relay.consume() }),
		]);
	};
	try {
		// Still dialing.
		expect(source.out.status.peek()).toBe("connecting");

		let [client, server] = await dial();
		await waitFor(source.out.status, (status) => status === "live");

		// The viewer's connection drops, though the publisher never stopped.
		client.abort();
		server.abort();
		await waitFor(source.out.status, (status) => status === "connecting");
		expect(route.broadcast.closed.peek()).toBeUndefined();

		[client, server] = await dial();
		await waitFor(source.out.status, (status) => status === "live");
		expect(seen.has("offline")).toBe(false);

		// Only the publisher stopping, with a session there to say so, reads offline.
		route.close();
		await waitFor(source.out.status, (status) => status === "offline");
		client.abort();
		server.abort();
	} finally {
		watching.close();
		source.close();
		producer.finish();
		route.close();
		viewer.close();
		relay.close();
	}
});

test("the badge says CONNECTING or DISCONNECTED in place of OFFLINE, and a refused token says why", async () => {
	// The only DOM these components touch.
	const made: { style: { display?: string }; dataset: Record<string, string>; textContent: string }[] = [];
	const original = globalThis.document;
	globalThis.document = {
		createElement: () => {
			const element = {
				style: {},
				dataset: {},
				textContent: "",
				className: "",
				type: "",
				title: "",
				setAttribute: () => {},
				append: () => {},
				appendChild: () => {},
				addEventListener: () => {},
				removeEventListener: () => {},
			};
			made.push(element);
			return element;
		},
	} as unknown as Document;

	// A connection that is redialing, or that stopped on a refused token: no session, so the
	// broadcast reads connecting.
	const watch = {
		connection: {
			url: new Signal(url),
			status: new Signal<string>("connecting"),
			error: new Signal<Error | undefined>(undefined),
		},
		broadcast: { out: { status: new Signal("connecting"), error: new Signal(undefined) } },
		sync: { out: { delay: new Signal({ asMillis: () => 0 }) } },
	} as unknown as MoqWatch;
	const state = { tab: new Signal("latency"), panel: new Signal(false) } as never;

	// `@moq/ui` defines a custom element at import, so give it a base class first.
	const element = globalThis.HTMLElement;
	globalThis.HTMLElement ??= class {} as unknown as typeof HTMLElement;
	const { liveBadge } = await import("./ui/components/live-badge");

	const effect = new Effect();
	type Made = (typeof made)[number];
	try {
		const badge = liveBadge(effect, watch, state) as unknown as Made;
		const offline = offlineIndicator(effect, watch) as unknown as Made;
		const refused = refusedIndicator(effect, watch) as unknown as Made;
		const flush = async () => {
			for (let i = 0; i < 5; i++) await Promise.resolve();
		};

		for (const status of ["connecting", "disconnected"]) {
			(watch.connection.status as Signal<string>).set(status);
			await flush();
			expect(badge.dataset.variant).toBe(status === "connecting" ? "connecting" : "error");
			expect(badge.style.display).toBe("");
			expect(offline.style.display).toBe("none");
		}

		// Connected, with the relay's announcements still on their way.
		(watch.connection.status as Signal<string>).set("connected");
		await flush();
		expect(badge.dataset.variant).toBe("connecting");
		expect(offline.style.display).toBe("none");

		// The relay refused the token, so the loop stopped.
		(watch.connection.status as Signal<string>).set("disconnected");
		(watch.connection.error as Signal<Error | undefined>).set(new Error("unauthorized"));
		await flush();
		expect(refused.style.display).toBe("");
		expect(made.some((el) => el.textContent === "Could not connect: unauthorized")).toBe(true);
		expect(offline.style.display).toBe("none");
	} finally {
		effect.close();
		globalThis.document = original;
		globalThis.HTMLElement = element;
	}
});
