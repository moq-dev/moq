/**
 * Steering wheel + pedal support via the Gamepad API.
 *
 * A wheel is fundamentally different from the keyboard mapping in
 * {@link module:input}: the keys are *hold*-style nudges that integrate over
 * time, while a wheel and its pedals are **absolute** - the rim angle *is* the
 * steering angle and a pedal's travel *is* the throttle. So this module maps
 * positions straight through rather than integrating them, and the only
 * time-domain shaping is a generous rate limit ({@link WHEEL_RATES}) that a
 * human hand can't hit in normal driving. It exists to smooth two things: a
 * violent yank of the rim, and the step that happens the instant the wheel
 * takes over from the keyboard.
 *
 * Browsers report wheels with no consistent axis order, polarity, or range -
 * it varies by model, driver, and OS. So there is a default layout that fits
 * the common Logitech/Thrustmaster arrangement, plus a calibration pass that
 * learns the real layout by watching the driver move each control. Learned
 * profiles are stored per device id in `localStorage`.
 *
 * @module
 */

import { z } from "zod/mini";
import { moveToward } from "./input.ts";
import type { DriveState } from "./schemas.ts";

/** How one physical axis maps onto a normalized control. */
export const AxisMapSchema = z.object({
	/** Index into `Gamepad.axes`. */
	axis: z.number(),
	/** Raw value at rest: wheel centered, pedal up. */
	rest: z.number(),
	/** Raw value at full travel: full right lock, pedal floored. */
	full: z.number(),
});
export type AxisMap = z.infer<typeof AxisMapSchema>;

/** A complete device layout: which axis/button drives which control. */
export const WheelProfileSchema = z.object({
	steer: AxisMapSchema,
	throttle: AxisMapSchema,
	brake: AxisMapSchema,
	/** `Gamepad.buttons` indices; omitted when the driver skipped the binding. */
	gearUp: z.optional(z.number()),
	gearDown: z.optional(z.number()),
	estop: z.optional(z.number()),
});
export type WheelProfile = z.infer<typeof WheelProfileSchema>;

/**
 * The layout most direct-input wheels report before calibration: rim on axis 0
 * centered at zero, pedals on axes 1 and 2 resting at +1 and reading -1 when
 * floored, shift paddles on buttons 4 and 5. This is a starting guess, not a
 * promise - {@link Calibration} replaces it per device.
 */
export const DEFAULT_PROFILE: WheelProfile = {
	steer: { axis: 0, rest: 0, full: 1 },
	throttle: { axis: 1, rest: 1, full: -1 },
	brake: { axis: 2, rest: 1, full: -1 },
	gearUp: 4,
	gearDown: 5,
};

/**
 * Rate limits in normalized units per second, deliberately much faster than
 * {@link module:input.RATES}: the wheel is absolute, so clamping it hard would
 * desync the rim from the commanded angle - the driver's hands would say one
 * thing and the truck another. These only bite on a yank or a source handoff.
 */
export const WHEEL_RATES = {
	/** Full lock to full lock in ~1.3 s. */
	steer: 1.5,
	/** A floored pedal reaches full requested throttle in ~0.67 s. */
	throttle: 1.5,
	/** Brake application stays fast. */
	brake: 3,
};

/** Ignore rim wobble this close to center, and rescale so full lock still reaches 1. */
export const STEER_DEADZONE = 0.03;
/** Pedal travel below this reads as fully released... */
export const PEDAL_DEADZONE = 0.05;
/** ...and above this as fully applied, so worn pedals can still reach the stops. */
export const PEDAL_SATURATION = 0.95;

/** Movement (in normalized units) that counts as the driver touching the wheel. */
export const MOVE_EPSILON = 0.02;

/**
 * The wheel refuses to take over from the keyboard while the throttle is
 * depressed, so a pedal resting under someone's foot can never be what hands
 * control to the wheel. Applies to the handoff only; once armed, the pedal is
 * honored normally.
 */
export const ARM_MAX_THROTTLE = 0.05;

/** A control that can be bound to a wheel button. */
export type WheelButton = "gearUp" | "gearDown" | "estop";

/** One poll of the wheel, already normalized into drive units. */
export interface WheelSample {
	/** -1 (full left) .. 1 (full right). */
	steering: number;
	/** 0 .. 1. */
	throttle: number;
	/** 0 .. 1. */
	brake: number;
	/** Buttons that went down on *this* poll, as rising edges. */
	pressed: ReadonlySet<WheelButton>;
	/** Whether any axis moved perceptibly since the previous poll. */
	moved: boolean;
}

/** The subset of `Gamepad` this module reads, so tests can pass plain objects. */
export interface PadLike {
	id: string;
	index: number;
	axes: readonly number[];
	buttons: readonly { pressed: boolean }[];
	mapping?: string;
	connected?: boolean;
}

function clamp(v: number, lo: number, hi: number): number {
	return Math.min(hi, Math.max(lo, v));
}

/** Normalize a raw axis into 0..1 of its calibrated travel (may over/undershoot). */
export function readAxis(axes: readonly number[], map: AxisMap): number {
	const raw = axes[map.axis];
	const span = map.full - map.rest;
	if (raw === undefined || Math.abs(span) < 1e-6) return 0;
	return (raw - map.rest) / span;
}

/** Read the rim as signed steering, with a rescaled center deadzone. */
export function readSteer(axes: readonly number[], map: AxisMap): number {
	const v = clamp(readAxis(axes, map), -1, 1);
	const mag = Math.abs(v);
	if (mag <= STEER_DEADZONE) return 0;
	return Math.sign(v) * ((mag - STEER_DEADZONE) / (1 - STEER_DEADZONE));
}

/** Read a pedal as 0..1, with rescaled deadzone and saturation at the stops. */
export function readPedal(axes: readonly number[], map: AxisMap): number {
	const v = clamp(readAxis(axes, map), 0, 1);
	if (v <= PEDAL_DEADZONE) return 0;
	if (v >= PEDAL_SATURATION) return 1;
	return (v - PEDAL_DEADZONE) / (PEDAL_SATURATION - PEDAL_DEADZONE);
}

/**
 * Move the drive state toward the wheel's absolute position over `dt` seconds.
 *
 * Mirrors {@link module:input.applyKeys} in shape so the caller can swap input
 * sources, but the semantics are the opposite: the sample is a *target*, not a
 * nudge, and the rate limit is only a safety ceiling on how fast it is chased.
 */
export function applyWheel(
	current: DriveState,
	sample: Pick<WheelSample, "steering" | "throttle" | "brake">,
	dt: number,
): Pick<DriveState, "steering" | "throttle" | "brake"> {
	return {
		steering: moveToward(current.steering, clamp(sample.steering, -1, 1), WHEEL_RATES.steer * dt),
		throttle: moveToward(current.throttle, clamp(sample.throttle, 0, 1), WHEEL_RATES.throttle * dt),
		brake: moveToward(current.brake, clamp(sample.brake, 0, 1), WHEEL_RATES.brake * dt),
	};
}

/** Which input source currently owns the drive axes. */
export type InputSource = "keyboard" | "wheel";

/** The outcome of one arbitration tick. */
export interface Arbitration {
	source: InputSource;
	/**
	 * True on the single tick an armed wheel disappeared, so the caller can fail
	 * safe exactly once rather than pinning the axes every frame afterwards.
	 */
	lost: boolean;
}

/**
 * Decide which source owns the axes this tick.
 *
 * The keyboard holds them by default. A wheel takes over only once it moves
 * *and* its throttle is released, so a pedal resting under a foot can never be
 * what hands it control. Handing control back to the keyboard is the caller's
 * job (any drive key does it), since the wheel can't observe that.
 */
export function arbitrate(current: InputSource, sample: WheelSample | undefined): Arbitration {
	if (!sample) return { source: "keyboard", lost: current === "wheel" };
	if (current !== "wheel" && sample.moved && sample.throttle <= ARM_MAX_THROTTLE) {
		return { source: "wheel", lost: false };
	}
	return { source: current, lost: false };
}

/**
 * The command to fall back to when an armed wheel vanishes mid-drive.
 *
 * Losing the wheel is not the same as releasing the keys. Held keys are a
 * steady state the driver chose and can still change; a wheel that has
 * unplugged has taken the control surface that was setting throttle with it, so
 * holding its last command is the runaway case. Cut the throttle and brake.
 *
 * Steering deliberately *holds* rather than centering the way e-stop does: the
 * driver has lost their input device, and trading uncommanded throttle for an
 * uncommanded swerve mid-corner is not an improvement.
 */
export function failSafe(current: DriveState): Pick<DriveState, "steering" | "throttle" | "brake"> {
	return { steering: current.steering, throttle: 0, brake: 1 };
}

/** Device-id fragments that identify a wheel outright, lowercased. */
const WHEEL_HINTS = [
	"wheel",
	"racing",
	"driving force",
	"g25",
	"g27",
	"g29",
	"g920",
	"g923",
	"g pro",
	"logitech momo",
	"thrustmaster",
	"t150",
	"t248",
	"t300",
	"t500",
	"tmx",
	"fanatec",
	"clubsport",
	"csl elite",
	"simucube",
	"moza",
];

/**
 * Whether a pad is plausibly a wheel.
 *
 * Wheels rarely fit the browser's "standard" gamepad mapping, so anything
 * unmapped with a rim plus two pedals' worth of axes qualifies even when the id
 * is unfamiliar. A false positive here costs a calibration pass, not safety -
 * the wheel still has to move before it takes control of anything.
 */
export function looksLikeWheel(pad: PadLike): boolean {
	const id = pad.id.toLowerCase();
	if (WHEEL_HINTS.some((hint) => id.includes(hint))) return true;
	return pad.mapping !== "standard" && pad.axes.length >= 3;
}

/** Read the connected pads, tolerating browsers without the Gamepad API. */
function scanPads(): PadLike[] {
	if (typeof navigator === "undefined" || !navigator.getGamepads) return [];
	const pads: PadLike[] = [];
	for (const pad of navigator.getGamepads()) {
		if (pad?.connected) pads.push(pad);
	}
	return pads;
}

function storage(): Storage | undefined {
	// Private browsing modes can throw on access rather than return null.
	try {
		return window.localStorage;
	} catch {
		return undefined;
	}
}

const STORAGE_PREFIX = "carla.wheel:";

/**
 * Tracks the connected wheel, its calibration, and its per-frame readings.
 *
 * Polling (rather than listening for `gamepadconnected`) keeps this in step
 * with the driving loop and handles the browser's requirement that a pad only
 * becomes visible after the user actually moves it.
 */
export class Wheel {
	#index: number | undefined;
	#id: string | undefined;
	#profile: WheelProfile = DEFAULT_PROFILE;
	#calibrated = false;
	#previous: { steering: number; throttle: number; brake: number } | undefined;
	#buttons: boolean[] = [];

	/** The connected wheel's device id, or undefined when none is present. */
	get id(): string | undefined {
		return this.#id;
	}

	/** Whether the active profile was learned from this device rather than guessed. */
	get calibrated(): boolean {
		return this.#calibrated;
	}

	/** The live pad, for the calibration UI. */
	pad(): PadLike | undefined {
		if (this.#index === undefined) return undefined;
		return scanPads().find((pad) => pad.index === this.#index && pad.id === this.#id);
	}

	/**
	 * Read the wheel's current position, or undefined when none is connected.
	 *
	 * Call once per frame: the rising-edge button set and the `moved` flag are
	 * both differences against the previous call.
	 */
	poll(): WheelSample | undefined {
		const pads = scanPads();
		let pad = pads.find((candidate) => candidate.index === this.#index && candidate.id === this.#id);
		if (!pad && this.#index !== undefined) {
			// Report the armed device as lost for one full arbitration tick before
			// adopting a replacement. Otherwise another connected wheel can inherit
			// control (and its current pedal positions) without independently arming.
			this.#forget();
			return undefined;
		}
		pad ??= pads.find(looksLikeWheel);
		if (!pad) return undefined;
		if (this.#index === undefined) this.#adopt(pad);

		const steering = readSteer(pad.axes, this.#profile.steer);
		const throttle = readPedal(pad.axes, this.#profile.throttle);
		const brake = readPedal(pad.axes, this.#profile.brake);

		// The first poll of a device only establishes the baseline: merely
		// plugging a wheel in must not be what takes control away from the
		// keyboard, so `moved` stays false until there is something to compare.
		const previous = this.#previous;
		const moved =
			previous !== undefined &&
			(Math.abs(previous.steering - steering) > MOVE_EPSILON ||
				Math.abs(previous.throttle - throttle) > MOVE_EPSILON ||
				Math.abs(previous.brake - brake) > MOVE_EPSILON);
		this.#previous = { steering, throttle, brake };

		const pressed = new Set<WheelButton>();
		for (const [control, index] of this.#bindings()) {
			const down = pad.buttons[index]?.pressed ?? false;
			if (down && !this.#buttons[index]) pressed.add(control);
		}
		this.#buttons = pad.buttons.map((button) => button.pressed);

		return { steering, throttle, brake, pressed, moved };
	}

	/** Install a freshly calibrated profile and persist it for this device. */
	save(profile: WheelProfile) {
		this.#profile = profile;
		this.#calibrated = true;
		// The old baseline was computed through the old profile, so comparing
		// against it would report a large synthetic "movement" on the next poll
		// and arm the wheel without the driver having touched it.
		this.#previous = undefined;
		if (!this.#id) return;
		try {
			storage()?.setItem(STORAGE_PREFIX + this.#id, JSON.stringify(profile));
		} catch {
			// A full or unavailable store just costs the driver a recalibration.
		}
	}

	*#bindings(): Generator<[WheelButton, number]> {
		const { gearUp, gearDown, estop } = this.#profile;
		if (gearUp !== undefined) yield ["gearUp", gearUp];
		if (gearDown !== undefined) yield ["gearDown", gearDown];
		if (estop !== undefined) yield ["estop", estop];
	}

	#adopt(pad: PadLike) {
		this.#index = pad.index;
		this.#id = pad.id;
		this.#previous = undefined;
		// Baseline the buttons from the device itself rather than from nothing:
		// a paddle already held when the wheel is adopted is a held button, not
		// a press, and must not read as a rising edge on the first poll.
		this.#buttons = pad.buttons.map((button) => button.pressed);

		const stored = loadProfile(pad.id);
		this.#profile = stored ?? DEFAULT_PROFILE;
		this.#calibrated = stored !== undefined;
	}

	#forget() {
		this.#index = undefined;
		this.#id = undefined;
		this.#profile = DEFAULT_PROFILE;
		this.#calibrated = false;
		this.#previous = undefined;
		this.#buttons = [];
	}
}

function loadProfile(id: string): WheelProfile | undefined {
	let raw: string | null | undefined;
	try {
		raw = storage()?.getItem(STORAGE_PREFIX + id);
	} catch {
		return undefined;
	}
	if (!raw) return undefined;
	try {
		const parsed = WheelProfileSchema.safeParse(JSON.parse(raw));
		return parsed.success ? parsed.data : undefined;
	} catch {
		// Corrupt JSON; the driver can recalibrate.
		return undefined;
	}
}

/** Minimum axis travel that counts as "the driver moved this control". */
export const CAPTURE_THRESHOLD = 0.25;

/**
 * Pick the axis that moved furthest from its resting value.
 *
 * Wheels cross-talk - turning the rim wiggles the pedal axes a little - so the
 * winner is the largest deviation rather than the first one over the line, and
 * axes already bound to another control are excluded.
 */
export function pickAxis(
	baseline: readonly number[],
	current: readonly number[],
	exclude: ReadonlySet<number> = new Set(),
): number | undefined {
	let best: number | undefined;
	let bestDelta = CAPTURE_THRESHOLD;
	for (let axis = 0; axis < current.length; axis++) {
		if (exclude.has(axis)) continue;
		const delta = Math.abs((current[axis] ?? 0) - (baseline[axis] ?? 0));
		if (delta > bestDelta) {
			best = axis;
			bestDelta = delta;
		}
	}
	return best;
}

/** Pick the first button that went down since the baseline snapshot. */
export function pickButton(
	baseline: readonly boolean[],
	current: readonly boolean[],
	exclude: ReadonlySet<number> = new Set(),
): number | undefined {
	for (let index = 0; index < current.length; index++) {
		if (exclude.has(index)) continue;
		if (current[index] && !baseline[index]) return index;
	}
	return undefined;
}

/** One prompt in the calibration sequence. */
export interface CalibrationStep {
	key: "rest" | "steer" | "throttle" | "brake" | WheelButton;
	kind: "rest" | "axis" | "button";
	prompt: string;
	/** Whether the driver may move past this step without binding anything. */
	optional: boolean;
}

export const CALIBRATION_STEPS: readonly CalibrationStep[] = [
	{
		key: "rest",
		kind: "rest",
		prompt: "Center the wheel and take your feet off the pedals, then press Capture.",
		optional: false,
	},
	{
		key: "steer",
		kind: "axis",
		prompt: "Hold the wheel at full RIGHT lock, then press Capture.",
		optional: false,
	},
	{
		key: "throttle",
		kind: "axis",
		prompt: "Press the THROTTLE pedal all the way down and hold it, then press Capture.",
		optional: false,
	},
	{
		key: "brake",
		kind: "axis",
		prompt: "Press the BRAKE pedal all the way down and hold it, then press Capture.",
		optional: false,
	},
	{
		key: "gearUp",
		kind: "button",
		prompt: "Press the paddle or button for SHIFT UP.",
		optional: true,
	},
	{
		key: "gearDown",
		kind: "button",
		prompt: "Press the paddle or button for SHIFT DOWN.",
		optional: true,
	},
	{
		key: "estop",
		kind: "button",
		prompt: "Press the button you want to use for E-STOP.",
		optional: true,
	},
];

/**
 * The calibration walk: snapshot the device at rest, then learn one control per
 * step from how far it moves away from that snapshot.
 *
 * Kept free of DOM so the sequencing is testable; the UI feeds it {@link update}
 * once per frame and calls {@link capture} / {@link skip} from its buttons.
 */
export class Calibration {
	#step = 0;
	#axes: number[] = [];
	#buttons: boolean[] = [];
	#restAxes: number[] = [];
	/** Buttons held when the current step began, so a held paddle can't self-bind. */
	#stepButtons: boolean[] = [];
	#draft: Partial<WheelProfile> = {};

	/** The step awaiting input, or undefined once calibration is complete. */
	get step(): CalibrationStep | undefined {
		return CALIBRATION_STEPS[this.#step];
	}

	get index(): number {
		return this.#step;
	}

	get total(): number {
		return CALIBRATION_STEPS.length;
	}

	get done(): boolean {
		return this.#step >= CALIBRATION_STEPS.length;
	}

	/** Feed the live pad state; call once per frame while calibrating. */
	update(pad: PadLike) {
		this.#axes = [...pad.axes];
		this.#buttons = pad.buttons.map((button) => button.pressed);

		// Button steps bind on a rising edge - there is nothing to press Capture
		// with when both hands are on a paddle.
		const step = this.step;
		if (step?.kind === "button") {
			const index = pickButton(this.#stepButtons, this.#buttons, this.#usedButtons());
			if (index !== undefined) {
				this.#draft[step.key as WheelButton] = index;
				this.#advance();
			}
		}
	}

	/**
	 * Commit the current step from the latest {@link update}.
	 *
	 * Returns false when the control didn't move far enough to identify - the UI
	 * leaves the step up so the driver can push the pedal harder and retry.
	 */
	capture(): boolean {
		const step = this.step;
		if (!step) return false;

		if (step.kind === "rest") {
			this.#restAxes = [...this.#axes];
			this.#advance();
			return true;
		}

		if (step.kind === "axis") {
			const axis = pickAxis(this.#restAxes, this.#axes, this.#usedAxes());
			if (axis === undefined) return false;
			const map: AxisMap = {
				axis,
				rest: this.#restAxes[axis] ?? 0,
				full: this.#axes[axis] ?? 0,
			};
			this.#draft[step.key as "steer" | "throttle" | "brake"] = map;
			this.#advance();
			return true;
		}

		// Button steps bind from update(); Capture just moves on.
		return this.skip();
	}

	/** Leave the current step unbound, if it is allowed to be. */
	skip(): boolean {
		const step = this.step;
		if (!step?.optional) return false;
		this.#advance();
		return true;
	}

	/** The finished profile, or undefined if the walk isn't complete. */
	result(): WheelProfile | undefined {
		const { steer, throttle, brake } = this.#draft;
		if (!this.done || !steer || !throttle || !brake) return undefined;
		return {
			steer,
			throttle,
			brake,
			gearUp: this.#draft.gearUp,
			gearDown: this.#draft.gearDown,
			estop: this.#draft.estop,
		};
	}

	#advance() {
		this.#step++;
		// Re-baseline held buttons so the press that ended one step doesn't also
		// satisfy the next one.
		this.#stepButtons = [...this.#buttons];
	}

	#usedAxes(): Set<number> {
		const used = new Set<number>();
		for (const map of [this.#draft.steer, this.#draft.throttle, this.#draft.brake]) {
			if (map) used.add(map.axis);
		}
		return used;
	}

	#usedButtons(): Set<number> {
		const used = new Set<number>();
		for (const index of [this.#draft.gearUp, this.#draft.gearDown, this.#draft.estop]) {
			if (index !== undefined) used.add(index);
		}
		return used;
	}
}

/** A short human-readable device name, since ids carry vendor/product noise. */
export function shortName(id: string): string {
	const trimmed = id.replace(/\s*\((?:Vendor|STANDARD|Product).*$/i, "").trim();
	return trimmed.length > 0 ? trimmed : id;
}
