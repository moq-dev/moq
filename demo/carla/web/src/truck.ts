/**
 * The non-UI truck steering session backend.
 *
 * Shaped like moq-boy's `Game`: a video pipeline pulled from `@moq/watch`, a
 * `control` track we publish, and a `status` track we subscribe to for
 * round-trip confirmation. The UI layer reads signals from this class to render
 * video, the control panel, and telemetry.
 *
 * @module
 */
import { Milli, Nano } from "@moq/clock";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import * as Watch from "@moq/watch";
import { changed, REST, takeoverState } from "./input.ts";
import { driverPath, parseCamera } from "./paths.ts";
import type { DriveState, Gear, TruckStatus } from "./schemas.ts";
import { TruckStatusSchema } from "./schemas.ts";

/** Configuration for creating a Truck session. */
export interface TruckConfig {
	/** Broadcast path as announced by the relay, e.g. "carla/truck1/front.hang". */
	broadcastId: Moq.Path.Valid;
	/** MoQ connection to the relay. */
	connection: Moq.Connection;
	/** Shared signal tracking which truck is currently being driven. */
	driving: Moq.Signals.Signal<string | undefined>;
	/** Shared signal tracking which truck is currently focused. */
	focused: Moq.Signals.Signal<string | undefined>;
	/** Only the hovered camera streams a fleet preview. */
	hovered?: Moq.Signals.Signal<string | undefined>;
}

/** A control command with captured timestamps, published via the command signal. */
interface Command {
	cmd: Record<string, unknown>;
	sequence: number;
	timestamps: { label: string; ts: number }[];
}

/** A retained safety snapshot that fans out to every truck consumer. */
export class SafetyCommandBroadcast {
	readonly broadcast: Moq.Broadcast.Producer;
	readonly #producer: Json.Snapshot.Producer<Record<string, unknown>>;

	constructor(command: Command, broadcast = new Moq.Broadcast.Producer()) {
		this.broadcast = broadcast;
		const track = this.broadcast.createTrack("control", { compress: true });
		this.#producer = new Json.Snapshot.Producer<Record<string, unknown>>({ track });
		this.#producer.update({
			value: { ...command.cmd, sequence: command.sequence, timestamps: command.timestamps },
		});
	}

	close() {
		this.#producer.finish();
		this.broadcast.close();
	}
}

const CONTROL_HEARTBEAT = Nano.fromMilli(Milli(250));
const MAX_PENDING_ACKS = 64;

/**
 * A single truck streaming + steering session (the non-UI backend).
 */
export class Truck {
	/** Stable per-camera id, e.g. "truck1/front". */
	readonly id: string;
	/** The truck this camera belongs to, shared by all of its cameras' control. */
	readonly name: string;
	readonly broadcastId: Moq.Path.Valid;
	readonly #signals = new Moq.Signals.Effect();
	readonly #driving: Moq.Signals.Signal<string | undefined>;

	// Reactive state exposed to the UI.
	readonly active = new Moq.Signals.Signal(false);
	readonly status = new Moq.Signals.Signal<TruckStatus | undefined>(undefined);
	readonly statusState = new Moq.Signals.Signal<"pending" | "available" | "unavailable">("pending");
	readonly driverId = new Moq.Signals.Signal<string | undefined>(undefined);
	readonly estopped = new Moq.Signals.Signal(false);
	readonly respawning = new Moq.Signals.Signal(false);
	readonly roundTripMs = new Moq.Signals.Signal<number | undefined>(undefined);

	// The driver's locally-requested drive state (what we are sending).
	readonly drive = new Moq.Signals.Signal<DriveState>({ ...REST });

	// Watch API objects, exposed so the UI can attach a canvas.
	readonly broadcast: Watch.Broadcast;
	readonly sync: Watch.Sync;
	readonly videoSource: Watch.Video.Source;
	readonly videoDecoder: Watch.Video.Decoder;
	readonly videoRenderer: Watch.Video.Renderer;

	// Internal control publishing state.
	#command = new Moq.Signals.Signal<Command | undefined>(undefined);
	#safetyCommand = new Moq.Signals.Signal<Command | undefined>(undefined);
	#sequence = 0;
	#sentAt = new Map<number, number>();
	#sawStatusSource = false;
	/** This player's e-stop, not yet echoed in a status. */
	#estopUnconfirmed = false;

	// The canvas is created by the UI layer after construction, so the renderer
	// takes it as a signal input rather than a property set afterwards.
	readonly #canvas = new Moq.Signals.Signal<HTMLCanvasElement | undefined>(undefined);

	constructor(config: TruckConfig) {
		const { broadcastId, connection, driving, focused, hovered } = config;
		const camera = parseCamera(broadcastId);
		if (!camera) throw new Error(`not a camera broadcast: ${broadcastId}`);
		this.broadcastId = broadcastId;
		this.id = camera.id;
		this.name = camera.truck;
		this.#driving = driving;

		// Active when this truck is the one being driven.
		this.#signals.run(this.#runActive.bind(this));
		this.#signals.run(this.#runControlLease.bind(this));
		this.#signals.run(this.#runRespawn.bind(this));
		this.#signals.interval(() => this.#heartbeat(), CONTROL_HEARTBEAT);

		// Video pipeline.
		this.broadcast = new Watch.Broadcast({
			origin: connection.origin,
			name: broadcastId,
			enabled: true,
		});
		this.#signals.cleanup(() => this.broadcast.close());

		// The source produces the jitter estimate Sync reads, so it is built first
		// to avoid a construction cycle.
		const videoTarget = new Moq.Signals.Computed<Watch.Video.Target>((effect) => ({
			name: effect.get(focused) === this.id ? "hd" : "sd",
		}));
		this.#signals.cleanup(() => videoTarget.close());
		this.videoSource = new Watch.Video.Source({
			broadcast: this.broadcast,
			target: videoTarget,
			supported: Watch.Video.Decoder.supported,
		});
		this.#signals.cleanup(() => this.videoSource.close());

		// Drive-by-wire wants the equivalent of `<moq-watch delay="instant">`: a
		// zero jitter buffer plus an unpaced decoder, so each frame paints the moment
		// it decodes instead of waiting on the playback clock. There is no audio to
		// keep in step.
		this.sync = new Watch.Sync({ delay: "instant" });
		this.#signals.cleanup(() => this.sync.close());

		// Keep status subscribed while idle; only an inspected camera owns video demand.
		const videoEnabled = new Moq.Signals.Signal(false);
		this.#signals.run((effect) => {
			const panel = effect.get(focused);
			videoEnabled.set(panel === this.id || (!panel && hovered !== undefined && effect.get(hovered) === this.id));
		});
		this.videoDecoder = new Watch.Video.Decoder({
			source: this.videoSource,
			sync: this.sync,
			enabled: videoEnabled,
		});
		this.#signals.cleanup(() => this.videoDecoder.close());

		this.videoRenderer = new Watch.Video.Renderer({ decoder: this.videoDecoder, canvas: this.#canvas });
		this.#signals.cleanup(() => this.videoRenderer.close());

		// Subscribe to the truck's status (telemetry) track.
		this.#signals.run(this.#runStatus.bind(this));

		// Publish control while this truck is being driven.
		this.#signals.run(this.#runControl.bind(this, connection));
		this.#signals.run(this.#runSafetyControl.bind(this, connection));
	}

	/** Attach the canvas the UI created so the renderer can draw into it. */
	setCanvas(canvas: HTMLCanvasElement | undefined) {
		this.#canvas.set(canvas);
	}

	/** Prepare a bumpless handoff from the latest vehicle feedback. */
	beginTakeover(): boolean {
		const status = this.status.peek();
		if (this.estopped.peek() || !status) return false;
		this.drive.set(takeoverState(status.measured));
		this.#sentDrive = undefined;
		return true;
	}

	/** Merge a partial drive update and publish it. */
	setDrive(patch: Partial<DriveState>) {
		if (this.estopped.peek() || this.respawning.peek()) return;
		const next = { ...this.drive.peek(), ...patch };
		// The local state ALWAYS advances. Only the wire is throttled: a caller
		// integrating at frame rate hands us deltas far below the epsilon, and
		// dropping those on the floor would leave steering motionless.
		this.drive.set(next);
		if (this.#sentDrive && this.#sentDrive.gear === next.gear && !changed(this.#sentDrive, next)) return;
		this.#sendDrive(next);
	}

	/** Publish a drive command and remember it as the epsilon's baseline. */
	#sendDrive(state: DriveState) {
		// Copy: the heartbeat hands us the signal's live object, and an in-place
		// mutation of it would leave the epsilon comparing a value to itself.
		this.#sentDrive = { ...state };
		this.#send({ type: "drive", ...state });
	}

	/** Request an emergency stop. */
	/**
	 * Latch e-stop on this truck even without holding the driving lease. A viewer
	 * sends it on a one-shot safety broadcast, leaving the driving session alone.
	 */
	emergencyStop() {
		this.respawning.set(false);
		this.drive.set({ ...REST });
		this.#sentDrive = undefined;
		this.#estopUnconfirmed = true;
		this.estopped.set(true);
		if (this.active.peek()) {
			this.#send({ type: "estop" });
		} else {
			this.#sendSafety({ type: "estop" });
		}
	}

	/** Toggle the safety action exposed by a truck tile. */
	toggleEmergencyStop() {
		if (this.estopped.peek() || this.status.peek()?.estopped) {
			this.resetEstop();
		} else {
			this.emergencyStop();
		}
	}

	/** Explicitly clear the e-stop latch into the full-brake state. */
	resetEstop() {
		// Wait for the authoritative echo. Otherwise a quick e-stop then reset can
		// replace the stop command before the truck ever receives and latches it.
		if (!this.status.peek()?.estopped) return;
		this.respawning.set(false);
		this.drive.set({ ...REST });
		this.#sentDrive = undefined;
		this.estopped.set(false);
		if (this.active.peek()) {
			this.#send({ type: "reset_estop" });
		} else {
			this.#sendSafety({ type: "reset_estop" });
		}
	}

	/** Return the truck to a spawn point while this player holds its lease. */
	respawn(): boolean {
		const status = this.status.peek();
		if (
			!this.active.peek() ||
			this.respawning.peek() ||
			this.estopped.peek() ||
			!status ||
			status.estopped ||
			status.driver !== this.driverId.peek()
		) {
			return false;
		}
		// Pause drive traffic until status confirms the server cleared the lease.
		// Otherwise the heartbeat could reclaim and brake the fresh spawn.
		this.respawning.set(true);
		this.drive.set({ ...REST });
		this.#sentDrive = undefined;
		this.#send({ type: "respawn" });
		return true;
	}

	/** The drive state last put on the wire, so the publish epsilon measures against it. */
	#sentDrive: DriveState | undefined;

	#send(cmd: Record<string, unknown>) {
		if (cmd.type === "estop" || cmd.type === "reset_estop") this.#safetyCommand.set(undefined);
		const command = this.#makeCommand(cmd);
		this.#sentAt.set(command.sequence, performance.now());
		while (this.#sentAt.size > MAX_PENDING_ACKS) {
			const oldest = this.#sentAt.keys().next().value;
			if (oldest == null) break;
			this.#sentAt.delete(oldest);
		}
		this.#command.set(command, true);
	}

	#sendSafety(cmd: Record<string, unknown>) {
		this.#safetyCommand.set(this.#makeCommand(cmd), true);
	}

	#makeCommand(cmd: Record<string, unknown>): Command {
		this.#sequence = (this.#sequence % Number.MAX_SAFE_INTEGER) + 1;
		return { cmd, sequence: this.#sequence, timestamps: this.#timestamps() };
	}

	#heartbeat() {
		if (!this.active.peek() || this.respawning.peek()) return;
		if (this.estopped.peek()) {
			this.#send({ type: "estop" });
		} else {
			// Also the floor under the publish epsilon: whatever it held back
			// reaches the truck within one heartbeat.
			this.#sendDrive(this.drive.peek());
		}
	}

	/** Collect media timestamps at each pipeline stage for latency measurement. */
	#timestamps(): { label: string; ts: number }[] {
		const entries: { label: string; ts: number }[] = [];
		const received = this.sync.out.timestamp.peek();
		if (received != null) entries.push({ label: "received", ts: received });
		const decoded = this.videoDecoder.out.timestamp.peek();
		if (decoded != null) entries.push({ label: "decoded", ts: decoded });
		const rendered = this.videoRenderer.out.timestamp.peek();
		if (rendered != null) entries.push({ label: "rendered", ts: rendered });
		return entries;
	}

	close() {
		this.#signals.close();
	}

	// --- Effect callbacks ---

	#runActive(effect: Moq.Signals.Effect) {
		this.active.set(effect.get(this.#driving) === this.id);
	}

	#runControlLease(effect: Moq.Signals.Effect) {
		if (!effect.get(this.active)) {
			// Releasing control closes the driving broadcast, which may not have
			// delivered this player's e-stop yet: carry it on the safety broadcast,
			// which stays up until a status echoes the latch.
			if (this.#estopUnconfirmed) this.#sendSafety({ type: "estop" });
			this.respawning.set(false);
			this.drive.set({ ...REST });
			this.#command.set(undefined);
			return;
		}

		// Inactive already reset the state. Resetting again on activation races
		// the input that claimed control: held keys recover on the next frame, but
		// an edge-triggered gear press would be silently lost.
		this.#heartbeat();
	}

	#runRespawn(effect: Moq.Signals.Effect) {
		if (!effect.get(this.respawning)) return;
		const status = effect.get(this.status);
		const driverId = effect.get(this.driverId);
		if (!status || status.driver === driverId) return;
		this.respawning.set(false);
		if (this.#driving.peek() === this.id) this.#driving.set(undefined);
	}

	#runStatus(effect: Moq.Signals.Effect) {
		const active = effect.get(this.broadcast.out.active);
		this.status.set(undefined);
		if (!active) {
			this.statusState.set(this.#sawStatusSource ? "unavailable" : "pending");
			return;
		}
		this.#sawStatusSource = true;
		this.statusState.set("pending");

		const statusTrack = active.track("status").subscribe({ priority: 10 });
		effect.cleanup(() => statusTrack.close());

		const consumer = new Json.Snapshot.Consumer({
			track: statusTrack,
			schema: TruckStatusSchema,
		});

		effect.spawn(async () => {
			for (;;) {
				let status: TruckStatus | undefined;
				try {
					status = (await consumer.latest())?.value;
					if (effect.abort.aborted) break;
				} catch (err) {
					if (effect.abort.aborted) break;
					// Refuse a malformed track rather than guess: control stays
					// unavailable until the broadcast is announced afresh.
					console.warn("Invalid status JSON:", err);
					this.status.set(undefined);
					this.statusState.set("unavailable");
					break;
				}
				if (!status) {
					this.status.set(undefined);
					this.statusState.set("unavailable");
					break;
				}
				// A status sent before the truck saw this player's e-stop must not
				// unlatch it: the heartbeat would then send drive frames, and a
				// snapshot track keeps only the newest, so the stop could be lost.
				if (status.estopped) this.#estopUnconfirmed = false;
				if (!this.#estopUnconfirmed) this.estopped.set(status.estopped);
				this.#acknowledgeSafety(status);
				this.#acknowledge(status);
				this.status.set(status);
				this.statusState.set("available");
			}
		});
	}

	#runControl(connection: Moq.Connection, effect: Moq.Signals.Effect) {
		const conn = effect.get(connection.origin);
		if (!conn) return;

		if (!effect.get(this.active)) return;

		const driverId = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
		this.driverId.set(driverId);

		const driverBroadcastPublication = conn.createBroadcast(driverPath(this.name, driverId));
		const driverBroadcast = driverBroadcastPublication.broadcast;
		effect.cleanup(() => {
			driverBroadcast.close();
			this.driverId.set(undefined);
			this.roundTripMs.set(undefined);
			this.#sentAt.clear();
		});

		const track = driverBroadcast.createTrack("control", { compress: true });
		const producer = new Json.Snapshot.Producer<Record<string, unknown>>({ track });
		effect.cleanup(() => producer.finish());
		effect.run(this.#runControlTrack.bind(this, track, producer));
		// A fresh epoch per lease: the truck binds its single driver to this path and epoch.
		driverBroadcastPublication.announce({ epoch: Moq.Epoch.mint() });
	}

	#runSafetyControl(connection: Moq.Connection, effect: Moq.Signals.Effect) {
		const conn = effect.get(connection.origin);
		const command = effect.get(this.#safetyCommand);
		if (!conn || !command) return;

		const driverId = `safety-${crypto.randomUUID().replaceAll("-", "").slice(0, 12)}`;
		const broadcastPublication = conn.createBroadcast(driverPath(this.name, driverId));
		const broadcast = broadcastPublication.broadcast;
		const safety = new SafetyCommandBroadcast(command, broadcast);
		broadcastPublication.announce({ epoch: Moq.Epoch.mint() });
		effect.cleanup(() => safety.close());
	}

	#runControlTrack(
		track: Moq.Track.Producer,
		producer: Json.Snapshot.Producer<Record<string, unknown>>,
		effect: Moq.Signals.Effect,
	) {
		if (effect.get(track.closed) !== undefined) return;

		const command = effect.get(this.#command);
		if (!command) return;

		producer.update({ value: { ...command.cmd, sequence: command.sequence, timestamps: command.timestamps } });
	}

	#acknowledge(status: TruckStatus) {
		if (status.driver !== this.driverId.peek() || status.sequence == null) return;
		const sentAt = this.#sentAt.get(status.sequence);
		if (sentAt == null) return;
		this.roundTripMs.set(performance.now() - sentAt);
		for (const sequence of this.#sentAt.keys()) {
			if (sequence <= status.sequence) this.#sentAt.delete(sequence);
		}
	}

	#acknowledgeSafety(status: TruckStatus) {
		const command = this.#safetyCommand.peek();
		if (!command) return;
		const type = command.cmd.type;
		if ((type === "estop" && status.estopped) || (type === "reset_estop" && !status.estopped)) {
			this.#safetyCommand.set(undefined);
		}
	}
}

/** Cycle gear N → F → R → N for a UI button. */
export function nextGear(gear: Gear): Gear {
	return gear === "neutral" ? "forward" : gear === "forward" ? "reverse" : "neutral";
}

/** Gear order for the shift paddles, from reverse up through forward. */
const GEARS: readonly Gear[] = ["reverse", "neutral", "forward"];

/**
 * Step one gear in `direction` without wrapping, for a wheel's shift paddles.
 *
 * Unlike {@link nextGear}, this never rolls forward straight into reverse - a
 * paddle at the end of its range does nothing rather than slamming the
 * transmission through neutral.
 */
export function shiftGear(gear: Gear, direction: 1 | -1): Gear {
	const index = GEARS.indexOf(gear);
	if (index < 0) return gear;
	return GEARS[Math.max(0, Math.min(GEARS.length - 1, index + direction))] ?? gear;
}
