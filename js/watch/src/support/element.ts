import { Effect, Signal } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import { type Codec, type Full, isSupported, type Partial } from "./";

// https://bugzilla.mozilla.org/show_bug.cgi?id=1967793
const isFirefox = navigator.userAgent.toLowerCase().includes("firefox");

// Themed pill button matching the player UI, with a hover highlight.
function stylePill(effect: Effect, button: HTMLButtonElement) {
	Object.assign(button.style, {
		display: "inline-flex",
		alignItems: "center",
		gap: "0.35rem",
		fontSize: "0.75rem",
		fontWeight: "500",
		color: "#ffffff",
		background: "rgba(255, 255, 255, 0.1)",
		border: "none",
		borderRadius: "0.375rem",
		padding: "0.35rem 0.75rem",
		cursor: "pointer",
		transition: "background-color 120ms",
	});
	effect.event(button, "mouseenter", () => {
		button.style.background = "rgba(255, 255, 255, 0.2)";
	});
	effect.event(button, "mouseleave", () => {
		button.style.background = "rgba(255, 255, 255, 0.1)";
	});
}

type Level = "ok" | "warn" | "no";
const LEVEL_COLOR: Record<Level, string> = { ok: "#22c55e", warn: "#facc15", no: "#f87171" };

function svg(paths: string, color: string, size = 14, width = 2.5): string {
	return `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="${color}" stroke-width="${width}" stroke-linecap="round" stroke-linejoin="round">${paths}</svg>`;
}

// Status glyphs: check (ok) / dash (warn) / cross (no), tinted by level.
const LEVEL_PATHS: Record<Level, string> = {
	ok: '<path d="M20 6 9 17l-5-5"/>',
	warn: '<path d="M5 12h14"/>',
	no: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
};
const ICON_CHEVRON_DOWN = '<path d="m6 9 6 6 6-6"/>';
const ICON_CHEVRON_UP = '<path d="m18 15-6-6-6 6"/>';
const ICON_CLOSE = '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>';

function glyph(raw: string): HTMLElement {
	const span = DOM.create("span", { style: { display: "inline-flex", flexShrink: "0" } });
	span.innerHTML = raw;
	return span;
}

// A right-aligned "<icon> label" status cell.
function statusCell(level: Level, label: string): HTMLElement {
	const cell = DOM.create("div", {
		style: {
			display: "flex",
			alignItems: "center",
			gap: "0.35rem",
			justifyContent: "flex-end",
			whiteSpace: "nowrap",
		},
	});
	cell.append(glyph(svg(LEVEL_PATHS[level], LEVEL_COLOR[level])), DOM.create("span", {}, label));
	return cell;
}

const OBSERVED = ["show", "details"] as const;
type Observed = (typeof OBSERVED)[number];

// Whether to display the support banner.
// - "always": Always display the banner.
// - "warning": Display the banner if a required feature needs a polyfill/fallback.
// - "error": Display the banner if a required feature is unsupported.
// - "never": Never display the banner.
export type Show = "always" | "warning" | "error" | "never";

export default class MoqWatchSupport extends HTMLElement {
	#show = new Signal<Show>("warning");
	#details = new Signal<boolean>(false);
	#support = new Signal<Full | undefined>(undefined);
	#close = new Signal<boolean>(false);

	#signals?: Effect;

	static observedAttributes = OBSERVED;

	constructor() {
		super();

		isSupported()
			.then((s) => this.#support.set(s))
			.catch((err) => console.error("Failed to detect watch support:", err));
	}

	connectedCallback() {
		this.#signals = new Effect();
		this.#signals.run(this.#render.bind(this));
	}

	disconnectedCallback() {
		this.#signals?.close();
		this.#signals = undefined;
	}

	attributeChangedCallback(name: Observed, _oldValue: string | null, newValue: string | null) {
		if (name === "show") {
			const show = newValue ?? "warning";
			if (show === "always" || show === "warning" || show === "error" || show === "never") {
				this.show = show;
			} else {
				throw new Error(`Invalid show: ${show}`);
			}
		} else if (name === "details") {
			this.details = newValue !== null;
		} else {
			const exhaustive: never = name;
			throw new Error(`Invalid attribute: ${exhaustive}`);
		}
	}

	get show(): Show {
		return this.#show.peek();
	}

	set show(show: Show) {
		this.#show.set(show);
	}

	get details(): boolean {
		return this.#details.peek();
	}

	set details(details: boolean) {
		this.#details.set(details);
	}

	#getSummary(support: Full): Partial {
		if (support.webtransport === "none") return "none";

		if (!support.audio.decoding || !support.video.decoding) return "none";
		if (!support.audio.render || !support.video.render) return "none";

		if (!Object.values(support.audio.decoding).some((v) => v === true || v === "full" || v === "partial"))
			return "none";
		if (!Object.values(support.video.decoding).some((v) => v.software || v.hardware)) return "none";

		if (!Object.values(support.audio.decoding).every((v) => v === true || v === "full")) return "partial";
		if (!Object.values(support.video.decoding).every((v) => v.software || v.hardware)) return "partial";

		return "full";
	}

	#render(effect: Effect) {
		const support = effect.get(this.#support);
		if (!support) return;

		const close = effect.get(this.#close);
		if (close) return;

		const show = effect.get(this.#show);
		if (show === "never") return;

		const summary = this.#getSummary(support);

		// Don't render the banner if we have full support and they only asked for warnings.
		if (show === "warning" && summary === "full") return;

		// Don't render the banner if we have at least partial support and they only asked for errors.
		if (show === "error" && summary !== "none") return;

		const container = DOM.create("div", {
			style: {
				margin: "1rem auto",
				maxWidth: "28rem",
				padding: "1rem",
				background: "rgba(18, 18, 20, 0.92)",
				border: "1px solid rgba(255, 255, 255, 0.15)",
				borderRadius: "0.75rem",
				boxShadow: "0 0.5rem 2rem rgba(0, 0, 0, 0.6)",
				color: "#ffffff",
				fontFamily: "system-ui, sans-serif",
				fontSize: "0.875rem",
			},
		});

		this.appendChild(container);
		effect.cleanup(() => this.removeChild(container));

		this.#renderHeader(container, summary, effect);

		if (effect.get(this.#details)) {
			this.#renderDetails(container, support, effect);
		}
	}

	#renderHeader(parent: HTMLDivElement, summary: Partial, effect: Effect) {
		const headerDiv = DOM.create("div", {
			style: {
				display: "flex",
				flexDirection: "row",
				gap: "1rem",
				flexWrap: "wrap",
				justifyContent: "space-between",
				alignItems: "center",
			},
		});

		const statusDiv = DOM.create("div", {
			style: { display: "flex", alignItems: "center", gap: "0.4rem", fontWeight: "bold" },
		});
		const statusLevel: Level = summary === "full" ? "ok" : summary === "partial" ? "warn" : "no";
		const statusText =
			summary === "full"
				? "Full Browser Support"
				: summary === "partial"
					? "Partial Browser Support"
					: "No Browser Support";
		statusDiv.append(
			glyph(svg(LEVEL_PATHS[statusLevel], LEVEL_COLOR[statusLevel], 16)),
			DOM.create("span", {}, statusText),
		);

		const detailsButton = DOM.create("button", { type: "button" });
		stylePill(effect, detailsButton);

		effect.event(detailsButton, "click", () => {
			this.#details.update((prev) => !prev);
		});

		effect.run((effect) => {
			const open = effect.get(this.#details);
			detailsButton.replaceChildren(
				glyph(svg(open ? ICON_CHEVRON_UP : ICON_CHEVRON_DOWN, "currentColor", 14, 2)),
				DOM.create("span", {}, "Details"),
			);
		});

		const closeButton = DOM.create("button", { type: "button" });
		closeButton.append(glyph(svg(ICON_CLOSE, "currentColor", 14, 2)), DOM.create("span", {}, "Close"));
		stylePill(effect, closeButton);

		effect.event(closeButton, "click", () => {
			this.#close.set(true);
		});

		headerDiv.appendChild(statusDiv);
		headerDiv.appendChild(detailsButton);
		headerDiv.appendChild(closeButton);

		parent.appendChild(headerDiv);
		effect.cleanup(() => parent.removeChild(headerDiv));
	}

	#renderDetails(parent: HTMLDivElement, support: Full, effect: Effect) {
		const container = DOM.create("div", {
			style: {
				display: "grid",
				gridTemplateColumns: "1fr auto",
				alignItems: "center",
				columnGap: "1rem",
				rowGap: "0.4rem",
				backgroundColor: "rgba(255, 255, 255, 0.05)",
				borderRadius: "0.5rem",
				padding: "0.75rem",
				marginTop: "0.75rem",
				fontSize: "0.8125rem",
			},
		});

		const binary = (value: boolean | undefined): [Level, string] => (value ? ["ok", "Yes"] : ["no", "No"]);
		const hardware = (codec: Codec | undefined): [Level, string] =>
			codec?.hardware
				? ["ok", "Hardware"]
				: codec?.software
					? ["warn", `Software${isFirefox ? "*" : ""}`]
					: ["no", "No"];
		const partial = (value: Partial | undefined): [Level, string] =>
			value === "full" ? ["ok", "Full"] : value === "partial" ? ["warn", "Polyfill"] : ["no", "None"];

		const addRow = (label: string, [level, text]: [Level, string]) => {
			container.appendChild(DOM.create("div", { style: { color: "rgba(255, 255, 255, 0.7)" } }, label));
			container.appendChild(statusCell(level, text));
		};

		addRow("WebTransport", partial(support.webtransport));
		addRow("Audio render", binary(support.audio.render));
		addRow("Video render", binary(support.video.render));
		addRow("Opus decode", partial(support.audio.decoding.opus));
		addRow("AAC decode", binary(support.audio.decoding.aac));
		addRow("AV1 decode", hardware(support.video.decoding?.av1));
		addRow("H.265 decode", hardware(support.video.decoding?.h265));
		addRow("H.264 decode", hardware(support.video.decoding?.h264));
		addRow("VP9 decode", hardware(support.video.decoding?.vp9));
		addRow("VP8 decode", hardware(support.video.decoding?.vp8));

		if (isFirefox) {
			const noteDiv = DOM.create(
				"div",
				{
					style: {
						gridColumn: "1 / -1",
						marginTop: "0.25rem",
						fontSize: "0.75rem",
						fontStyle: "italic",
						color: "rgba(255, 255, 255, 0.6)",
					},
				},
				"Hardware acceleration is ",
				DOM.create(
					"a",
					{ href: "https://github.com/w3c/webcodecs/issues/896", style: { color: "#ff9900" } },
					"undetectable",
				),
				" on Firefox.",
			);
			container.appendChild(noteDiv);
		}

		parent.appendChild(container);
		effect.cleanup(() => parent.removeChild(container));
	}
}

customElements.define("moq-watch-support", MoqWatchSupport);

declare global {
	interface HTMLElementTagNameMap {
		"moq-watch-support": MoqWatchSupport;
	}
}
