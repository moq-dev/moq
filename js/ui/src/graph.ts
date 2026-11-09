import type { Effect } from "@moq/signals";

/** Options for {@link graph}. @internal */
export interface GraphOptions {
	/** Stroke/fill color for the line (any CSS color). */
	color?: string;
	/** Formats the latest value for the readout in the corner. */
	format?: (v: number) => string;
}

/** A sparkline created by {@link graph}. @internal */
export interface Graph {
	/** The root element to mount. */
	el: HTMLElement;
	/** Append a sample. Pass undefined to record a gap (drawn as zero). */
	push(value: number | undefined): void;
}

// Samples of history retained; older ones scroll off the left.
const CAPACITY = 120;

/** Normalize any CSS color and apply an alpha, so the gradient works for named/rgb/hsl inputs too. */
function withAlpha(color: string, alpha: number): string {
	const ctx = document.createElement("canvas").getContext("2d");
	if (!ctx) return color;
	ctx.fillStyle = color;
	const normalized = ctx.fillStyle; // canvas returns "#rrggbb" or "rgba(...)"
	if (normalized.startsWith("#")) {
		const n = Number.parseInt(normalized.slice(1), 16);
		return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
	}
	const parts = normalized.match(/[\d.]+/g);
	if (parts && parts.length >= 3) return `rgba(${parts[0]}, ${parts[1]}, ${parts[2]}, ${alpha})`;
	return color;
}

/**
 * A rolling time-series sparkline. Samples scroll right-to-left and the area
 * under the line is filled with a fading gradient. Redraws are event-driven:
 * each `push` and any canvas resize triggers a repaint (no animation loop).
 */
export function graph(parent: Effect, title: string, opts?: GraphOptions): Graph {
	const color = opts?.color ?? "#4ade80";
	// Precompute the gradient stops once (the color is fixed) instead of per frame.
	const fillTop = withAlpha(color, 0.33);
	const fillBottom = withAlpha(color, 0);

	const el = document.createElement("div");
	el.className = "graph";

	const header = document.createElement("div");
	header.className = "graph-header";
	const label = document.createElement("span");
	label.className = "graph-label";
	label.textContent = title;
	const value = document.createElement("span");
	value.className = "graph-value";
	value.style.color = color;
	header.append(label, value);

	const canvas = document.createElement("canvas");
	canvas.className = "graph-canvas";

	el.append(header, canvas);

	const samples: number[] = [];
	// Smoothed autoscale ceiling so the baseline doesn't jump every frame.
	let scale = 1;

	const draw = () => {
		const ctx = canvas.getContext("2d");
		if (ctx) {
			const dpr = window.devicePixelRatio || 1;
			const rect = canvas.getBoundingClientRect();
			const w = rect.width;
			const h = rect.height;
			const cw = Math.round(w * dpr);
			const ch = Math.round(h * dpr);
			if (canvas.width !== cw || canvas.height !== ch) {
				canvas.width = cw;
				canvas.height = ch;
			}

			ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
			ctx.clearRect(0, 0, w, h);

			if (w > 0 && h > 0) {
				// Target the rolling peak with headroom.
				const peak = samples.length ? Math.max(...samples) : 0;
				const target = Math.max(1, peak * 1.2);
				// Ease toward the target to avoid jitter.
				scale += (target - scale) * 0.1;

				const pad = 1;
				const usable = h - pad * 2;
				const x = (i: number) => (i / (CAPACITY - 1)) * w;
				const y = (v: number) => pad + usable - (Math.min(v, scale) / scale) * usable;

				// Offset so the newest sample sits at the right edge.
				const offset = CAPACITY - samples.length;

				if (samples.length > 1) {
					ctx.beginPath();
					ctx.moveTo(x(offset), y(samples[0]));
					for (let i = 1; i < samples.length; i++) ctx.lineTo(x(offset + i), y(samples[i]));

					// Fill under the curve.
					const grad = ctx.createLinearGradient(0, 0, 0, h);
					grad.addColorStop(0, fillTop);
					grad.addColorStop(1, fillBottom);
					ctx.save();
					ctx.lineTo(x(offset + samples.length - 1), h);
					ctx.lineTo(x(offset), h);
					ctx.closePath();
					ctx.fillStyle = grad;
					ctx.fill();
					ctx.restore();

					// Stroke the line.
					ctx.beginPath();
					ctx.moveTo(x(offset), y(samples[0]));
					for (let i = 1; i < samples.length; i++) ctx.lineTo(x(offset + i), y(samples[i]));
					ctx.strokeStyle = color;
					ctx.lineWidth = 1.5;
					ctx.lineJoin = "round";
					ctx.stroke();
				}
			}
		}
	};

	// Redraw when a new sample lands (the data rate is the frame rate for a
	// sparkline) and when the canvas is resized. No idle animation loop.
	const push = (v: number | undefined) => {
		samples.push(v !== undefined && Number.isFinite(v) ? Math.max(0, v) : 0);
		while (samples.length > CAPACITY) samples.shift();
		value.textContent = v !== undefined && Number.isFinite(v) ? (opts?.format?.(v) ?? v.toFixed(0)) : "-";
		draw();
	};

	if (typeof ResizeObserver !== "undefined") {
		const observer = new ResizeObserver(() => draw());
		observer.observe(canvas);
		parent.cleanup(() => observer.disconnect());
	}

	return { el, push };
}
