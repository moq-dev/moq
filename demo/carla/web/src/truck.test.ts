import { describe, expect, test } from "bun:test";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { applyKeys, REST } from "./input.ts";
import type { TruckStatus } from "./schemas.ts";
import { SafetyCommandBroadcast, shiftGear, Truck } from "./truck.ts";

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
		const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
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

	function statusOrigin() {
		const origin = new Moq.Broadcast.Producer();
		const track = origin.createTrack("status", { compress: true });
		const producer = new Json.Snapshot.Producer<TruckStatus>({ track });
		return { origin, producer };
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
		const { origin, producer } = statusOrigin();

		try {
			const available = truck.statusState.changed();
			(truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>).set(
				origin.consume(),
			);
			producer.update({ value: status(0.5) });
			expect(await available).toBe("available");
			expect(truck.status.peek()?.accepted.throttle).toBe(0.5);

			const unavailable = truck.statusState.changed();
			producer.finish();
			expect(await unavailable).toBe("unavailable");
			expect(truck.status.peek()).toBeUndefined();
		} finally {
			producer.finish();
			origin.close();
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
		const { origin, producer } = statusOrigin();
		const next = async (update: TruckStatus) => {
			const changed = truck.status.changed();
			producer.update({ value: update });
			await changed;
		};

		try {
			(truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>).set(
				origin.consume(),
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
			origin.close();
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
		const { origin, producer } = statusOrigin();
		const active = truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>;

		try {
			active.set(origin.consume());
			producer.update({ value: status(0.5) });
			while (truck.statusState.peek() !== "available") await truck.statusState.changed();

			const unavailable = truck.statusState.changed();
			active.set(undefined);
			expect(await unavailable).toBe("unavailable");
			expect(truck.status.peek()).toBeUndefined();
		} finally {
			producer.finish();
			origin.close();
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
		const first = statusOrigin();
		const second = statusOrigin();
		const active = truck.broadcast.out.active as Moq.Signals.Signal<Moq.Broadcast.Consumer | undefined>;

		try {
			active.set(first.origin.consume());
			first.producer.update({ value: status(0.25) });
			while (truck.statusState.peek() !== "available") await truck.statusState.changed();

			const transitions: string[] = [];
			const unsubscribe = truck.statusState.subscribe((state) => transitions.push(state));
			try {
				active.set(second.origin.consume());
				second.producer.update({ value: status(0.75) });
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
			first.origin.close();
			second.origin.close();
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

	test("preserves measured controls while claiming control", async () => {
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
			expect(t.drive.peek()).toEqual(measured);
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

	test("one safety snapshot reaches every current truck consumer", async () => {
		const safety = new SafetyCommandBroadcast({
			cmd: { type: "estop" },
			sequence: 7,
			timestamps: [],
		});
		const front = new Json.Snapshot.Consumer<Record<string, unknown>>({
			track: safety.broadcast.track("control").subscribe(),
		});
		const rear = new Json.Snapshot.Consumer<Record<string, unknown>>({
			track: safety.broadcast.track("control").subscribe(),
		});

		try {
			expect((await front.next())?.value).toEqual({ type: "estop", sequence: 7, timestamps: [] });
			expect((await rear.next())?.value).toEqual({ type: "estop", sequence: 7, timestamps: [] });
		} finally {
			safety.close();
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
				estop_by: "bystander",
				media_pipeline: {},
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
				driver: "someone-else",
				media_pipeline: {},
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
