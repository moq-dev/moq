import { describe, expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { applyKeys, REST } from "./input.ts";
import { driverPath } from "./paths.ts";
import type { TruckStatus } from "./schemas.ts";
import { shiftGear, Truck } from "./truck.ts";

// The status track is scale-less, so each write names a group key; keys only have to increase.
let keys = 0n;
const key = () => Moq.Time.Timestamp.unscaled(keys++);

describe("Truck video demand", () => {
	test("only hovered or focused cameras subscribe, and focus suppresses other previews", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const hovered = new Moq.Signals.Signal<string | undefined>(undefined);
		const trucks = ["carla/truck1/front.hang", "carla/truck2/front.hang"].map(
			(id) =>
				new Truck({
					broadcastId: Moq.Path.from(id),
					connection,
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
		}
	});
});

describe("Truck status", () => {
	function status(throttle: number): TruckStatus {
		return {
			accepted: { steering: 0.25, throttle, brake: 0, gear: "forward" },
			autonomy: "manual",
			estopped: false,
			media_pipeline: {},
		};
	}

	function statusBroadcast() {
		const broadcast = new Moq.Broadcast.Producer();
		const track = broadcast.createTrack("status", { compression: "deflate" });
		if (!track) throw new Error("the track already exists");
		const producer = new Json.Snapshot.Producer<TruckStatus>({ track });
		return { broadcast, producer };
	}

	test("clears valid telemetry when the status track ends", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			driving,
			focused,
		});
		const { broadcast, producer } = statusBroadcast();

		try {
			const available = truck.statusState.changed();
			(truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>).set(
				broadcast.consume(),
			);
			producer.update(status(0.5), key());
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
		}
	});

	test("a status from before the truck saw an e-stop does not unlatch it", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			driving,
			focused,
		});
		const { broadcast, producer } = statusBroadcast();
		const next = async (update: TruckStatus) => {
			const changed = truck.status.changed();
			producer.update(update, key());
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
		}
	});

	test("marks status unavailable when an active broadcast disappears", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			driving,
			focused,
		});
		const { broadcast, producer } = statusBroadcast();
		const active = truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>;

		try {
			active.set(broadcast.consume());
			producer.update(status(0.5), key());
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
		}
	});

	test("a canceled status consumer cannot overwrite its replacement", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			driving,
			focused,
		});
		const first = statusBroadcast();
		const second = statusBroadcast();
		const active = truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>;

		try {
			active.set(first.broadcast.consume());
			first.producer.update(status(0.25), key());
			while (truck.statusState.peek() !== "available") await truck.statusState.changed();

			const transitions: string[] = [];
			const unsubscribe = truck.statusState.subscribe((state) => transitions.push(state));
			try {
				active.set(second.broadcast.consume());
				second.producer.update(status(0.75), key());
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
		}
	});
});

describe("Truck.setDrive", () => {
	function truck() {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const t = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			driving,
			focused,
		});
		return { t, connection };
	}

	// Guards setDrive's contract: local state advances however small the step,
	// while the epsilon throttles only the wire. The frame loop's own test lives
	// in input.test.ts's stepDrive suite; this one calls setDrive directly and so
	// cannot catch a gate added at the call site.
	test("accumulates sub-epsilon frames instead of discarding them", () => {
		const { t, connection } = truck();
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
		}
	});

	test("a single sub-epsilon frame still moves the local state", () => {
		const { t, connection } = truck();
		try {
			t.setDrive(applyKeys(t.drive.peek(), new Set(["d"]), 1 / 60));
			expect(t.drive.peek().steering).toBeGreaterThan(0);
		} finally {
			t.close();
			connection.close();
		}
	});

	test("claims with measured steering and gear but zero throttle and full brake", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>("truck1/front");
		const t = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			driving,
			focused,
		});
		try {
			const measured = { steering: -0.35, throttle: 0.6, brake: 0.1, gear: "forward" } as const;
			t.status.set({
				accepted: { ...REST },
				autonomy: "autonomous",
				estopped: false,
				media_pipeline: {},
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
		}
	});

	test("refuses a takeover before the truck reports status", () => {
		const { t, connection } = truck();
		try {
			expect(t.beginTakeover()).toBe(false);
			expect(t.drive.peek()).toEqual(REST);
		} finally {
			t.close();
			connection.close();
		}
	});
});

describe("Truck safety controls", () => {
	test("a viewer e-stop does not claim the driving session", () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
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
		}
	});

	test("a focused viewer resets only after the truck confirms the latch", () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
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
				media_pipeline: {},
				estop_by: "bystander",
			});
			truck.resetEstop();
			expect(driving.peek()).toBeUndefined();
			expect(truck.estopped.peek()).toBe(false);
		} finally {
			truck.close();
			connection.close();
		}
	});

	test("the tile safety action exits a confirmed e-stop without claiming control", () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
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
				media_pipeline: {},
			});

			truck.toggleEmergencyStop();

			expect(truck.estopped.peek()).toBe(false);
			expect(driving.peek()).toBeUndefined();
		} finally {
			truck.close();
			connection.close();
		}
	});
});

describe("Truck control", () => {
	const decoder = new TextDecoder();
	const HEARTBEAT = Duration.millis(250);

	/** A truck on a connection lending `origin`, where the test plays the simulator and the clock. */
	function setup() {
		const clock = new Manual();
		const installed = clock.install();
		const origin = new Moq.Origin.Producer();
		// The truck reads only the origin, which a real connection lends while it has a relay.
		const connection = {
			origin: new Moq.Signals.Signal<Moq.Origin.Table | undefined>(origin),
		} as unknown as Moq.Connection;
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>(undefined);
		const camera = origin.route(Moq.Path.from("carla/truck1/front.hang"));
		const track = camera.broadcast.createTrack("status", { compression: "deflate" });
		if (!track) throw new Error("the status track already exists");
		const producer = new Json.Snapshot.Producer<TruckStatus>({ track });
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			driving,
			focused,
		});

		/** Publish a status, and wait until the truck has read it. */
		const echo = async (value: Partial<TruckStatus>) => {
			const status: TruckStatus = {
				accepted: { ...REST },
				autonomy: "manual",
				estopped: false,
				media_pipeline: {},
				...value,
			};
			producer.update(status, key());
			while (!Bun.deepEquals(truck.status.peek(), status)) await truck.status.changed();
		};
		const close = () => {
			truck.close();
			producer.finish();
			camera.close();
			origin.close();
			installed.close();
		};
		return { truck, driving, origin, clock, echo, close };
	}

	/** Read the `control` track at `path` oldest first, a heartbeat late like the truck. */
	async function control(origin: Moq.Origin.Producer, path: Moq.Path.Valid) {
		const request = origin.request(path);
		let broadcast = request.active.peek();
		while (!broadcast) broadcast = await request.active.changed();
		const track = broadcast.track("control");
		const reader = track
			.subscribe({ order: "ascending", ranges: [{ start: { live: { min: HEARTBEAT, max: HEARTBEAT } } }] })
			.ordered();
		const read = async () => {
			const frame = await reader.readFrame();
			return frame && JSON.parse(decoder.decode(frame.payload));
		};
		return { request, track, read };
	}

	/** Wait for the driver id the truck publishes its lease under. */
	async function drive(truck: Truck, driving: Moq.Signals.Signal<string | undefined>) {
		driving.set(truck.id);
		let id = truck.driverId.peek();
		while (!id) id = await truck.driverId.changed();
		return id;
	}

	test("a driver publishes each command as a group of its own, which the status acknowledges", async () => {
		const { truck, driving, origin, echo, close } = setup();
		try {
			const id = await drive(truck, driving);
			const { track, read } = await control(origin, driverPath("truck1", id));
			// A superseded command stays fresh for the truck's delay budget.
			expect((await track.info()).maxAge?.asMillis()).toBe(250);

			// Taking control heartbeats at once, so the truck hears the claim.
			const command = await read();
			expect(command.type).toBe("drive");

			const measured = truck.roundTripMs.changed();
			await echo({ driver: id, sequence: command.sequence });
			expect(await measured).toBeGreaterThanOrEqual(0);
		} finally {
			close();
		}
	});

	test("a reset right before a drive reaches a heartbeat-late reader whole", async () => {
		const { truck, driving, origin, echo, close } = setup();
		try {
			const id = await drive(truck, driving);
			const { read } = await control(origin, driverPath("truck1", id));
			expect((await read()).type).toBe("drive");
			await echo({ driver: id, estopped: true });

			// The clock is still, so no heartbeat lands between the two.
			truck.resetEstop();
			truck.setDrive({ throttle: 0.5 });
			expect((await read()).type).toBe("reset_estop");
			expect((await read()).type).toBe("drive");
		} finally {
			close();
		}
	});

	test("a claim the status has not confirmed yet is kept", async () => {
		const { truck, driving, echo, close } = setup();
		try {
			await drive(truck, driving);
			await echo({ driver: "someone-else" });
			await echo({});
			expect(driving.peek()).toBe(truck.id);
		} finally {
			close();
		}
	});

	test("the truck no longer naming its driver ends the lease and its broadcast", async () => {
		const { truck, driving, origin, echo, close } = setup();
		try {
			const id = await drive(truck, driving);
			const { request } = await control(origin, driverPath("truck1", id));
			await echo({ driver: id });

			// A stuck truck's respawn or the watchdog let go of it.
			const released = driving.changed();
			await echo({});
			expect(await released).toBeUndefined();
			while (truck.driverId.peek() !== undefined) await truck.driverId.changed();
			// The request settles once nothing serves the path any more.
			await request.closed;
		} finally {
			close();
		}
	});

	test("a bystander's e-stop publishes a broadcast of its own until the status echoes it", async () => {
		const { truck, driving, origin, echo, close } = setup();
		const announced = origin.announced(Moq.Path.Pattern.parse("viewer/carla/truck1/*"));
		try {
			truck.emergencyStop();
			const entry = await announced.next();
			if (!entry) throw new Error("no safety broadcast");
			expect(entry.prefix).toMatch(/^viewer\/carla\/truck1\/safety-[0-9a-f]{12}$/);
			const { request, read } = await control(origin, entry.prefix);
			expect((await read()).type).toBe("estop");
			expect(driving.peek()).toBeUndefined();
			expect(truck.driverId.peek()).toBeUndefined();

			// The truck's status echoing the latch is what lets the broadcast go.
			await echo({ estopped: true, estop_by: entry.prefix.split("/").at(-1) });
			// The request settles once nothing serves the path any more.
			await request.closed;
			expect(truck.estopped.peek()).toBe(true);
		} finally {
			announced.close();
			close();
		}
	});

	test("an e-stop the status never echoes stops showing as latched", async () => {
		const { truck, clock, close } = setup();
		try {
			truck.emergencyStop();
			expect(truck.estopped.peek()).toBe(true);
			await clock.advance(Duration.millis(1_999));
			expect(truck.estopped.peek()).toBe(true);
			await clock.advance(Duration.millis(1));
			expect(truck.estopped.peek()).toBe(false);
		} finally {
			close();
		}
	});
});

describe("Truck respawn", () => {
	test("requires the truck's current lease and clears old input", async () => {
		const connection = new Moq.Connection({ enabled: false });
		const driving = new Moq.Signals.Signal<string | undefined>(undefined);
		const focused = new Moq.Signals.Signal<string | undefined>("truck1/front");
		const truck = new Truck({
			broadcastId: Moq.Path.from("carla/truck1/front.hang"),
			connection,
			driving,
			focused,
		});

		try {
			const active = truck.active.changed();
			driving.set(truck.id);
			expect(await active).toBe(true);

			truck.driverId.set("mine");
			const status = {
				accepted: { ...REST },
				autonomy: "manual",
				estopped: false,
				media_pipeline: {},
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
