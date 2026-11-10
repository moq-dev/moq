// Replays of the OfflineMeansOffline and RefusalIsExplained paths in `verify/Watch.tla`: the
// player reads "offline" whenever it holds no broadcast handle, so the viewer is told OFFLINE
// while it is still connecting, after its own connection drops, and after the relay refused it,
// and the status badge that could say otherwise hides behind that notice.
import { expect, test } from "bun:test";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import { Origin, Path } from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import { accept, connect } from "../../net/src/connection/index.ts";
import * as Lite from "../../net/src/lite/index.ts";
import { createMockTransportPair, waitFor } from "../../net/src/mock.ts";
import { Broadcast } from "./broadcast";
import type MoqWatch from "./element";
import { offlineIndicator } from "./ui/components/offline-indicator";

const url = new URL("https://relay.example/room");

test("residual: a live broadcast reads offline until connected and again once the connection drops", async () => {
	// The publisher is live on the relay the whole time.
	const relay = new Origin.Producer();
	const name = Path.from("live.hang");
	const route = relay.route(name);
	const producer = new Json.Snapshot.Producer<Catalog.Root>({
		track: route.broadcast.createTrack(Catalog.TRACK),
		deltas: false,
	});
	producer.update({ value: {} });

	const viewer = new Origin.Producer();
	const source = new Broadcast({ origin: viewer, name, catalogFormat: "hang", enabled: true });
	try {
		// 0. Still dialing: OFFLINE.
		expect(source.out.status.peek()).toBe("offline");

		const pair = createMockTransportPair(Lite.ALPN_06);
		const [client, server] = await Promise.all([
			connect({ url, transport: pair.client, consume: viewer }),
			accept({ transport: pair.server, url, publish: relay.consume() }),
		]);
		await waitFor(source.out.status, (status) => status === "live");

		// The viewer's connection drops: OFFLINE, though the publisher never stopped.
		client.abort();
		server.abort();
		await waitFor(source.out.status, (status) => status === "offline");
		expect(route.broadcast.closed.peek()).toBeUndefined();
	} finally {
		source.close();
		producer.finish();
		route.close();
		viewer.close();
		relay.close();
	}
});

test("residual: the badge's CONNECTING and DISCONNECTED hide behind OFFLINE", async () => {
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

	// A connection that is redialing, or that stopped on a refused token: no session, so no
	// broadcast handle, so the broadcast reads offline.
	const watch = {
		connection: { url: new Signal(url), status: new Signal<string>("connecting") },
		broadcast: { out: { status: new Signal("offline") } },
		sync: { out: { delay: new Signal({ asMillis: () => 0 }) } },
	} as unknown as MoqWatch;
	const state = { tab: new Signal("latency"), panel: new Signal(false) } as never;

	// `@moq/ui` defines a custom element at import, so give it a base class first.
	const element = globalThis.HTMLElement;
	globalThis.HTMLElement ??= class {} as unknown as typeof HTMLElement;
	const { liveBadge } = await import("./ui/components/live-badge");

	const effect = new Effect();
	try {
		const badge = liveBadge(effect, watch, state) as unknown as (typeof made)[number];
		const offline = offlineIndicator(effect, watch) as unknown as (typeof made)[number];
		for (const status of ["connecting", "disconnected"]) {
			(watch.connection.status as Signal<string>).set(status);
			for (let i = 0; i < 5; i++) await Promise.resolve();
			expect(badge.dataset.variant).toBe(status === "connecting" ? "connecting" : "error");
			expect(badge.style.display).toBe("none");
			expect(offline.style.display).toBe("");
		}
	} finally {
		effect.close();
		globalThis.document = original;
		globalThis.HTMLElement = element;
	}
});
