/**
 * Backup-camera-style steering guides projected onto the video.
 *
 * Two curved rails that bend with a steering angle. The same projection draws
 * the driver's **requested input** (a local preview of what is being sent) and
 * the truck's **measured** steering off the status track, so the gap between
 * them is a shape on the road rather than two numbers to subtract.
 *
 * @module
 */
import type { DriveState } from "./schemas.ts";

export interface GuideStyle {
	/** Stroke color (CSS). */
	color: string;
	/** Line width in CSS pixels. */
	width: number;
	/** Dash pattern, or empty for solid. */
	dash?: number[];
	/** Overall opacity. */
	alpha?: number;
}

// How far the far end of the rails swings sideways at full lock, as a fraction
// of width. Tuned to read like a reversing camera, not a literal kinematic model.
const CURVE_GAIN = 0.34;

// Vertical extent of the projected ground plane (fractions of canvas height).
const Y_NEAR = 0.98;
const Y_FAR = 0.46;
// Half-width of the wheel track at the near and far ends (fractions of width).
const HALF_NEAR = 0.36;
const HALF_FAR = 0.06;

const SEGMENTS = 48;

/** One rail point. */
interface Pt {
	x: number;
	y: number;
}

/** Sample the left and right rails for a given steering angle. */
function rails(w: number, h: number, steering: number): { left: Pt[]; right: Pt[] } {
	const cx = w / 2;
	const left: Pt[] = [];
	const right: Pt[] = [];
	for (let i = 0; i < SEGMENTS; i++) {
		const t = i / (SEGMENTS - 1);
		// Perspective: rails converge and rise toward the horizon.
		const y = (Y_NEAR + (Y_FAR - Y_NEAR) * t) * h;
		const half = (HALF_NEAR + (HALF_FAR - HALF_NEAR) * t) * w;
		// Lateral swing grows with distance (t²) so the rails curve, not shear.
		const shift = steering * CURVE_GAIN * t * t * w;
		left.push({ x: cx - half + shift, y });
		right.push({ x: cx + half + shift, y });
	}
	return { left, right };
}

function stroke(ctx: CanvasRenderingContext2D, pts: Pt[]) {
	ctx.beginPath();
	pts.forEach((p, i) => {
		if (i) ctx.lineTo(p.x, p.y);
		else ctx.moveTo(p.x, p.y);
	});
	ctx.stroke();
}

/** Draw one trajectory (two rails). */
export function drawGuide(ctx: CanvasRenderingContext2D, w: number, h: number, drive: DriveState, style: GuideStyle) {
	const { left, right } = rails(w, h, drive.steering);

	ctx.save();
	ctx.globalAlpha = style.alpha ?? 1;
	ctx.lineJoin = "round";
	ctx.lineCap = "round";
	ctx.lineWidth = style.width;
	ctx.strokeStyle = style.color;
	ctx.setLineDash(style.dash ?? []);
	stroke(ctx, left);
	stroke(ctx, right);
	ctx.restore();
}

/**
 * Shade the ground between the requested trajectory and the measured one.
 *
 * The wedge is the difference itself: it closes to nothing while the truck is
 * tracking, and opens toward the side the truck is actually going. Drawing it
 * is what stops two nearly-identical curves from reading as one.
 */
export function drawDivergence(
	ctx: CanvasRenderingContext2D,
	w: number,
	h: number,
	requested: DriveState,
	measured: DriveState,
	style: { color: string; alpha?: number },
) {
	const want = rails(w, h, requested.steering);
	const got = rails(w, h, measured.steering);

	ctx.save();
	ctx.globalAlpha = style.alpha ?? 1;
	ctx.fillStyle = style.color;
	for (const [a, b] of [
		[want.left, got.left],
		[want.right, got.right],
	]) {
		ctx.beginPath();
		a.forEach((p, i) => {
			if (i) ctx.lineTo(p.x, p.y);
			else ctx.moveTo(p.x, p.y);
		});
		for (let i = b.length - 1; i >= 0; i--) ctx.lineTo(b[i].x, b[i].y);
		ctx.closePath();
		ctx.fill();
	}
	ctx.restore();
}

function pointAt(pts: Pt[], progress: number): Pt {
	const position = Math.max(0, Math.min(1, progress)) * (pts.length - 1);
	const before = Math.floor(position);
	const after = Math.min(pts.length - 1, before + 1);
	const mix = position - before;
	return {
		x: pts[before].x + (pts[after].x - pts[before].x) * mix,
		y: pts[before].y + (pts[after].y - pts[before].y) * mix,
	};
}

/** Sample a rail in either direction, including an exact endpoint. */
function portion(pts: Pt[], from: number, to: number): Pt[] {
	const count = Math.max(2, Math.ceil(Math.abs(to - from) * (pts.length - 1)) + 1);
	return Array.from({ length: count }, (_, i) => pointAt(pts, from + ((to - from) * i) / (count - 1)));
}

function centerline(left: Pt[], right: Pt[]): Pt[] {
	return left.map((point, index) => ({
		x: (point.x + right[index].x) / 2,
		y: (point.y + right[index].y) / 2,
	}));
}

function arrow(ctx: CanvasRenderingContext2D, pts: Pt[], color: string, width: number) {
	if (pts.length < 2) return;
	const tip = pts[pts.length - 1];
	const previous = pts[pts.length - 2];
	const angle = Math.atan2(tip.y - previous.y, tip.x - previous.x);
	const size = width * 1.55;

	ctx.save();
	ctx.lineCap = "round";
	ctx.lineJoin = "round";
	ctx.strokeStyle = "rgba(0, 0, 0, 0.72)";
	ctx.lineWidth = width + 4;
	stroke(ctx, pts);
	ctx.strokeStyle = color;
	ctx.lineWidth = width;
	stroke(ctx, pts);
	ctx.translate(tip.x, tip.y);
	ctx.rotate(angle);
	ctx.fillStyle = "rgba(0, 0, 0, 0.72)";
	ctx.beginPath();
	ctx.moveTo(size + 3, 0);
	ctx.lineTo(-size, size * 0.72 + 2);
	ctx.lineTo(-size, -size * 0.72 - 2);
	ctx.closePath();
	ctx.fill();
	ctx.fillStyle = color;
	ctx.beginPath();
	ctx.moveTo(size, 0);
	ctx.lineTo(-size, size * 0.72);
	ctx.lineTo(-size, -size * 0.72);
	ctx.closePath();
	ctx.fill();
	ctx.restore();
}

export interface LongitudinalIndicator {
	pedal: "throttle" | "brake";
	amount: number;
	from: number;
	to: number;
	source: "actual" | "requested";
}

function pedal(drive: DriveState): Pick<LongitudinalIndicator, "pedal" | "amount"> {
	// Braking wins if a physical controller briefly reports both pedals.
	return drive.brake > 0
		? { pedal: "brake", amount: Math.max(0, Math.min(1, drive.brake)) }
		: { pedal: "throttle", amount: Math.max(0, Math.min(1, drive.throttle)) };
}

/** Resolve the one arrow shown by the HUD, preferring the truck's measured state. */
export function longitudinalIndicator(
	requested: DriveState | undefined,
	measured: DriveState | undefined,
): LongitudinalIndicator | undefined {
	const drive = measured ?? requested;
	if (!drive) return undefined;
	const value = pedal(drive);
	const reversing = drive.gear === "reverse";
	const pointsAway = value.pedal === "throttle" ? !reversing : reversing;
	const start = pointsAway ? 0.1 : 0.9;
	const end = start + (pointsAway ? 0.8 : -0.8) * value.amount;
	return { ...value, from: start, to: end, source: measured ? "actual" : "requested" };
}

/** Draw one centered pedal arrow, using measured truck feedback when available. */
export function drawDriveHud(
	ctx: CanvasRenderingContext2D,
	w: number,
	h: number,
	requested: DriveState | undefined,
	measured: DriveState | undefined,
) {
	const indicator = longitudinalIndicator(requested, measured);
	if (!indicator) return;
	const drive = measured ?? requested;
	if (!drive) return;
	const scale = Math.max(0.78, Math.min(1.15, w / 900));
	const { left, right } = rails(w, h, drive.steering);
	const middle = centerline(left, right);
	const throttleColor = "#39d353";
	const brakeColor = "#ff4d55";
	const arrowColor =
		indicator.source === "requested" ? "#50c8ff" : indicator.pedal === "throttle" ? throttleColor : brakeColor;
	const barWidth = 7 * scale;

	ctx.save();
	if (indicator.amount > 0) arrow(ctx, portion(middle, indicator.from, indicator.to), arrowColor, barWidth);

	ctx.restore();
}
