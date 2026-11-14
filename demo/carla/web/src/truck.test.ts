import { describe, expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { applyKeys, REST } from "./input.ts";
import { handle, SOCKET } from "./paths.ts";
import type { TruckStatus } from "./schemas.ts";
import { shiftGear, Truck } from "./truck.ts";

// The status track is scale-less, so each write names a group key; keys only have to increase.
let keys = 0n;
const key = () => Moq.Time.Timestamp.unscaled(keys++);

describe("Truck video demand", () => {
	test("only hovered or focused cameras subscribe, and focus suppresses other previews", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const hovered = new Moq.Signals.Signal<string | undefined>(undefined);
		const trucks = ["carla/truck1/front.hang", "carla/truck2/front.hang"].map(
			(id) =>
				new Truck({
					broadcastId: Moq.Path.from(id),
					connection,
					origin,
					driving,
					focused,
					hovered,
				}),
		);
		const settle = yieldNow;
		const demand = () => trucks.map((truck) => truck.videoDecoder.in.enabled.peek());
		try {
			await settle();
			expect(demand()).toEqual([false, false]);
			hovered.set(trucks[0].id);
			await settle();
			expect(demand()).toEqual([true, false]);
			expect(trucks[0].videoSource.in.target.peek()).toEqual({ name: "sd" });
			hovered.set(trucks[1].id);
			await settle();
			expect(demand()).toEqual([false, true]);
			focused.set(trucks[0].id);
			await settle();
			expect(demand()).toEqual([true, false]);
			expect(trucks[0].videoSource.in.target.peek()).toEqual({ name: "hd" });
			focused.set(undefined);
			hovered.set(undefined);
			await settle();
			expect(demand()).toEqual([false, false]);
			expect(driving.peek()).toBeUndefined();
		} finally {
			for (const truck of trucks) truck.close();
			connection.close();
			origin.close();
		}
	});
});

describe("Truck status", () => {
	function status(throttle: number): TruckStatus {
		return {
			accepted: { steering: 0.25, throttle, brake: 0, gear: "forward" },
			autonomy: "manual",
			estopped: false,
		};
	}

	function statusBroadcast() {
		const broadcast = new Moq.Broadcast.Producer();
		const track = broadcast.createTrack("status", { compression: "deflate" });
		const producer = new Json.Snapshot.Producer<TruckStatus>({ track });
		return { broadcast, producer };
	}

	test("clears valid telemetry when the status track ends", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});
		const { broadcast, producer } = statusBroadcast();

		try {
			const available = truck.statusState.changed();
			(truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>).set(
				broadcast.consume(),
			);
			producer.update({ value: status(0.5), at: key() });
			expect(await available).toBe("available");
			expect(truck.status.peek()?.accepted.throttle).toBe(0.5);

			const unavailable = truck.statusState.changed();
			producer.finish();
			expect(await unavailable).toBe("unavailable");
			expect(truck.status.peek()).toBeUndefined();
		} finally {
			producer.finish();
			broadcast.close();
			truck.close();
			connection.close();
			origin.close();
		}
	});

	test("a status from before the truck saw an e-stop does not unlatch it", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});
		const { broadcast, producer } = statusBroadcast();
		const next = async (update: TruckStatus) => {
			const changed = truck.status.changed();
			producer.update({ value: update, at: key() });
			await changed;
		};

		try {
			(truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>).set(
				broadcast.consume(),
			);
			await next(status(0.5));
			truck.emergencyStop();
			await next(status(0.4));
			expect(truck.estopped.peek()).toBe(true);

			// Once echoed, the truck's status is authoritative again.
			await next({ ...status(0), estopped: true });
			await next(status(0));
			expect(truck.estopped.peek()).toBe(false);
		} finally {
			producer.finish();
			broadcast.close();
			truck.close();
			connection.close();
			origin.close();
		}
	});

	test("marks status unavailable when an active broadcast disappears", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});
		const { broadcast, producer } = statusBroadcast();
		const active = truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>;

		try {
			active.set(broadcast.consume());
			producer.update({ value: status(0.5), at: key() });
			while (truck.statusState.peek() !== "available") await truck.statusState.changed();

			const unavailable = truck.statusState.changed();
			active.set(undefined);
			expect(await unavailable).toBe("unavailable");
			expect(truck.status.peek()).toBeUndefined();
		} finally {
			producer.finish();
			broadcast.close();
			truck.close();
			connection.close();
			origin.close();
		}
	});

	test("a canceled status consumer cannot overwrite its replacement", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});
		const first = statusBroadcast();
		const second = statusBroadcast();
		const active = truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>;

		try {
			active.set(first.broadcast.consume());
			first.producer.update({ value: status(0.25), at: key() });
			while (truck.statusState.peek() !== "available") await truck.statusState.changed();

			const transitions: string[] = [];
			const unsubscribe = truck.statusState.subscribe((state) => transitions.push(state));
			try {
				active.set(second.broadcast.consume());
				second.producer.update({ value: status(0.75), at: key() });
				while (truck.status.peek()?.accepted.throttle !== 0.75) await truck.status.changed();
				await Promise.resolve();
				await Promise.resolve();
				expect(truck.statusState.peek()).toBe("available");
				expect(truck.status.peek()?.accepted.throttle).toBe(0.75);
				expect(transitions).not.toContain("unavailable");
			} finally {
				unsubscribe();
			}
		} finally {
			first.producer.finish();
			second.producer.finish();
			first.broadcast.close();
			second.broadcast.close();
			truck.close();
			connection.close();
			origin.close();
		}
	});
});

describe("Truck.setDrive", () => {
	function truck() {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const t = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});
		return { t, connection, origin };
	}

	// Guards setDrive's contract: local state advances however small the step,
	// while the epsilon throttles only the wire. The frame loop's own test lives
	// in input.test.ts's stepDrive suite; this one calls setDrive directly and so
	// cannot catch a gate added at the call site.
	test("accumulates sub-epsilon frames instead of discarding them", () => {
		const { t, connection, origin } = truck();
		try {
			const dt = 1 / 120;
			for (let i = 0; i < 120; i++) {
				const cur = t.drive.peek();
				t.setDrive(applyKeys(cur, new Set(["d"]), dt));
			}
			// One second at 120 Hz arrives below the publish epsilon every frame.
			expect(t.drive.peek().steering).toBeCloseTo(0.45, 2);
		} finally {
			t.close();
			connection.close();
			origin.close();
		}
	});

	test("a single sub-epsilon frame still moves the local state", () => {
		const { t, connection, origin } = truck();
		try {
			t.setDrive(applyKeys(t.drive.peek(), new Set(["d"]), 1 / 60));
			expect(t.drive.peek().steering).toBeGreaterThan(0);
		} finally {
			t.close();
			connection.close();
			origin.close();
		}
	});

	test("claims with measured steering and gear but zero throttle and full brake", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>("truck1/front");
		const t = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});
		try {
			const measured = { steering: -0.35, throttle: 0.6, brake: 0.1, gear: "forward" } as const;
			t.status.set({
				accepted: { ...REST },
				autonomy: "autonomous",
				estopped: false,
				measured,
			});
			expect(t.beginTakeover()).toBe(true);
			driving.set(t.id);
			await Promise.resolve();
			expect(t.active.peek()).toBe(true);
			expect(t.drive.peek()).toEqual({ ...measured, throttle: 0, brake: 1 });
		} finally {
			t.close();
			connection.close();
			origin.close();
		}
	});

	test("refuses a takeover before the truck reports status", () => {
		const { t, connection, origin } = truck();
		try {
			expect(t.beginTakeover()).toBe(false);
			expect(t.drive.peek()).toEqual(REST);
		} finally {
			t.close();
			connection.close();
			origin.close();
		}
	});
});

describe("Truck safety controls", () => {
	test("a viewer e-stop does not claim the driving session", () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});

		try {
			truck.emergencyStop();
			expect(driving.peek()).toBeUndefined();
			expect(truck.active.peek()).toBe(false);
			expect(truck.estopped.peek()).toBe(true);
		} finally {
			truck.close();
			connection.close();
			origin.close();
		}
	});

	test("a focused viewer resets only after the truck confirms the latch", () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});

		try {
			truck.estopped.set(true);
			truck.resetEstop();
			expect(truck.estopped.peek()).toBe(true);

			truck.status.set({
				accepted: { ...REST },
				autonomy: "unknown",
				estopped: true,
				estop_by: "bystander",
			});
			truck.resetEstop();
			expect(driving.peek()).toBeUndefined();
			expect(truck.estopped.peek()).toBe(false);
		} finally {
			truck.close();
			connection.close();
			origin.close();
		}
	});

	test("the tile safety action exits a confirmed e-stop without claiming control", () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});

		try {
			truck.toggleEmergencyStop();
			expect(truck.estopped.peek()).toBe(true);
			truck.status.set({
				accepted: { ...REST },
				autonomy: "manual",
				estopped: true,
			});

			truck.toggleEmergencyStop();

			expect(truck.estopped.peek()).toBe(false);
			expect(driving.peek()).toBeUndefined();
		} finally {
			truck.close();
			connection.close();
			origin.close();
		}
	});
});

describe("Truck sessions", () => {
	const decoder = new TextDecoder();
	const encoder = new TextEncoder();

	/** A truck whose socket endpoint accepts on the same origin as the player. */
	function setup() {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});
		const endpoint = new Moq.Socket.Endpoint(origin, SOCKET, "truck1");
		const close = () => {
			truck.close();
			endpoint.close();
			connection.close();
			origin.close();
		};
		return { truck, driving, endpoint, close };
	}

	async function read(stream: Moq.Socket.TrackStream, reader = stream.consumer.ordered()) {
		const frame = await reader.readFrame();
		return frame && JSON.parse(decoder.decode(frame.payload));
	}

	test("a driver's commands and the truck's acknowledgements travel on its own session", async () => {
		const { truck, driving, endpoint, close } = setup();
		try {
			const incoming = endpoint.acceptTrack();
			driving.set(truck.id);
			const dial = await incoming;
			expect(dial.peer).toMatch(/^driver-[0-9a-f]{32}$/);
			const stream = await dial.accept();
			// Status names the lease by the session, never by the endpoint id.
			while (truck.handle.peek() === undefined) await truck.handle.changed();
			expect(truck.handle.peek()).toBe(await handle(stream.session));

			// Taking control heartbeats at once, so the truck hears the claim.
			const command = await read(stream);
			expect(command.type).toBe("drive");

			const measured = truck.roundTripMs.changed();
			const pipeline = [{ label: "received→decoded", ms: 4 }];
			const reply = { sequence: command.sequence, media_pipeline: pipeline };
			const group = stream.producer.appendGroup();
			group.writeFrame({
				payload: encoder.encode(JSON.stringify(reply)),
				timestamp: Moq.Time.Timestamp.unscaled(group.sequence),
			});
			group.finish();
			expect(await measured).toBeGreaterThanOrEqual(0);
			expect(truck.pipeline.peek()).toEqual(pipeline);
		} finally {
			close();
		}
	});

	test("the truck ending the session ends the lease", async () => {
		const { truck, driving, endpoint, close } = setup();
		try {
			const incoming = endpoint.acceptTrack();
			driving.set(truck.id);
			const stream = await (await incoming).accept();
			await read(stream);

			const released = driving.changed();
			stream.producer.finish();
			expect(await released).toBeUndefined();
			// The lease's endpoint goes with it.
			while (truck.handle.peek() !== undefined) await truck.handle.changed();
		} finally {
			close();
		}
	});

	test("releasing control ends the session cleanly", async () => {
		const { truck, driving, endpoint, close } = setup();
		try {
			const incoming = endpoint.acceptTrack();
			driving.set(truck.id);
			const stream = await (await incoming).accept();
			const reader = stream.consumer.ordered();
			await read(stream, reader);

			driving.set(undefined);
			// Anything the heartbeat sent first, then the clean end.
			while ((await read(stream, reader)) !== undefined);
		} finally {
			close();
		}
	});

	test("a bystander's e-stop dials a session of its own without taking the lease", async () => {
		const { truck, driving, endpoint, close } = setup();
		try {
			const incoming = endpoint.acceptTrack();
			truck.emergencyStop();
			const dial = await incoming;
			expect(dial.peer).toMatch(/^driver-/);
			const stream = await dial.accept();
			const reader = stream.consumer.ordered();
			expect((await read(stream, reader)).type).toBe("estop");
			expect(driving.peek()).toBeUndefined();
			expect(truck.handle.peek()).toBeUndefined();

			// The truck's status echoing the latch is what lets the session go.
			const broadcast = new Moq.Broadcast.Producer();
			const producer = new Json.Snapshot.Producer<TruckStatus>({
				track: broadcast.createTrack("status", { compression: "deflate" }),
			});
			try {
				(truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>).set(
					broadcast.consume(),
				);
				producer.update({
					value: {
						accepted: { ...REST },
						autonomy: "manual",
						estopped: true,
						estop_by: await handle(dial.session),
					},
					at: key(),
				});
				expect(await read(stream, reader)).toBeUndefined();
			} finally {
				producer.finish();
				broadcast.close();
			}
		} finally {
			close();
		}
	});
});

describe("Truck safety sessions", () => {
	test("an e-stop whose session ends unconfirmed stops showing as latched", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});
		const endpoint = new Moq.Socket.Endpoint(origin, SOCKET, "truck1");
		try {
			const incoming = endpoint.acceptTrack();
			truck.emergencyStop();
			expect(truck.estopped.peek()).toBe(true);
			const stream = await (await incoming).accept();
			await stream.consumer.ordered().readFrame();

			// The truck goes away before its status ever echoed the latch.
			const cleared = truck.estopped.changed();
			stream.producer.finish();
			expect(await cleared).toBe(false);
		} finally {
			truck.close();
			endpoint.close();
			connection.close();
			origin.close();
		}
	});
});

describe("Truck respawn", () => {
	test("requires the truck's current lease and clears old input", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const origin = new Moq.Origin.Producer();
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>("truck1/front");
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			origin,
			driving,
			focused,
		});

		try {
			const active = truck.active.changed();
			driving.set(truck.id);
			expect(await active).toBe(true);

			truck.handle.set("mine");
			const status = {
				accepted: { ...REST },
				autonomy: "manual",
				estopped: false,
				driver: "someone-else",
			} satisfies TruckStatus;
			truck.status.set(status);
			expect(truck.respawn()).toBe(false);
			truck.status.set({ ...status, driver: "mine" });
			truck.estopped.set(true);
			expect(truck.respawn()).toBe(false);
			truck.estopped.set(false);
			truck.setDrive({ steering: 0.5, throttle: 0.8, brake: 0, gear: "forward" });

			expect(truck.respawn()).toBe(true);
			expect(truck.drive.peek()).toEqual(REST);
			expect(truck.respawning.peek()).toBe(true);
			truck.setDrive({ throttle: 1 });
			expect(truck.drive.peek()).toEqual(REST);

			const released = driving.changed();
			truck.status.set({ ...status, driver: undefined });
			expect(await released).toBeUndefined();
			expect(truck.respawning.peek()).toBe(false);
		} finally {
			truck.close();
			connection.close();
			origin.close();
		}
	});
});

describe("shiftGear", () => {
	test("steps one gear at a time", () => {
		expect(shiftGear("neutral", 1)).toBe("forward");
		expect(shiftGear("neutral", -1)).toBe("reverse");
		expect(shiftGear("reverse", 1)).toBe("neutral");
		expect(shiftGear("forward", -1)).toBe("neutral");
	});

	test("never wraps forward into reverse", () => {
		expect(shiftGear("forward", 1)).toBe("forward");
		expect(shiftGear("reverse", -1)).toBe("reverse");
	});
});
