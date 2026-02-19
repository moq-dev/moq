import { Effect, Signal } from "@moq/signals";
import * as DOM from "@moq/signals/dom";
import { type Codec, type Full, isSupported, type Partial } from "./";

// https://bugzilla.mozilla.org/show_bug.cgi?id=1967793
const isFirefox = navigator.userAgent.toLowerCase().includes("firefox");

const OBSERVED = ["show", "details"] as const;
type Observed = (typeof OBSERVED)[number];

// Whether to display the support banner.
// - "always": Always display the banner.
// - "warning": Display the banner if a required feature needs a polyfill/fallback.
// - "error": Display the banner if a required feature is unsupported.
// - "never": Never display the banner.
export type Show = "always" | "warning" | "error" | "never";

export default class MoqPublishSupport extends HTMLElement {
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
			.catch((err) => console.error("Failed to detect publish support:", err));
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

		if (!support.audio.encoding || !support.video.encoding) return "none";
		if (!support.audio.capture) return "none";

		if (!Object.values(support.audio.encoding).some((v) => v === true || v === "full" || v === "partial"))
			return "none";
		if (!Object.values(support.video.encoding).some((v) => v.software || v.hardware)) return "none";

		if (support.video.capture === "partial") return "partial";

		if (!Object.values(support.video.encoding).some((v) => v.hardware)) return "partial";

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
				margin: "0 auto",
				maxWidth: "28rem",
				padding: "1rem",
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
			style: { fontWeight: "bold" },
		});

		if (summary === "full") {
			statusDiv.textContent = "🟢 Full Browser Support";
		} else if (summary === "partial") {
			statusDiv.textContent = "🟡 Partial Browser Support";
		} else if (summary === "none") {
			statusDiv.textContent = "🔴 No Browser Support";
		}

		const detailsButton = DOM.create("button", {
			type: "button",
			style: { fontSize: "14px" },
		});

		effect.event(detailsButton, "click", () => {
			this.#details.update((prev) => !prev);
		});

		effect.run((effect) => {
			detailsButton.textContent = effect.get(this.#details) ? "Details ➖" : "Details ➕";
		});

		const closeButton = DOM.create(
			"button",
			{
				type: "button",
				style: { fontSize: "14px" },
			},
			"Close ❌",
		);

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
				gridTemplateColumns: "1fr 1fr 1fr",
				columnGap: "0.5rem",
				rowGap: "0.2rem",
				backgroundColor: "rgba(0, 0, 0, 0.6)",
				borderRadius: "0.5rem",
				padding: "1rem",
				fontSize: "0.875rem",
			},
		});

		const binary = (value: boolean | undefined) => (value ? "🟢 Yes" : "🔴 No");
		const hardware = (codec: Codec | undefined) =>
			codec?.hardware ? "🟢 Hardware" : codec?.software ? `🟡 Software${isFirefox ? "*" : ""}` : "🔴 No";
		const partial = (value: Partial | undefined) =>
			value === "full" ? "🟢 Full" : value === "partial" ? "🟡 Polyfill" : "🔴 None";

		const addRow = (label: string, col2: string, col3: string) => {
			const labelDiv = DOM.create(
				"div",
				{
					style: {
						gridColumnStart: "1",
						fontWeight: "bold",
						textAlign: "right",
					},
				},
				label,
			);

			const col2Div = DOM.create(
				"div",
				{
					style: {
						gridColumnStart: "2",
						textAlign: "center",
					},
				},
				col2,
			);

			const col3Div = DOM.create(
				"div",
				{
					style: { gridColumnStart: "3" },
				},
				col3,
			);

			container.appendChild(labelDiv);
			container.appendChild(col2Div);
			container.appendChild(col3Div);
		};

		addRow("WebTransport", "", partial(support.webtransport));
		addRow("Capture", "Audio", binary(support.audio.capture));
		addRow("", "Video", partial(support.video.capture));
		addRow("Encoding", "Opus", partial(support.audio.encoding.opus));
		addRow("", "AAC", binary(support.audio.encoding.aac));
		addRow("", "AV1", hardware(support.video.encoding?.av1));
		addRow("", "H.265", hardware(support.video.encoding?.h265));
		addRow("", "H.264", hardware(support.video.encoding?.h264));
		addRow("", "VP9", hardware(support.video.encoding?.vp9));
		addRow("", "VP8", hardware(support.video.encoding?.vp8));

		if (isFirefox) {
			const noteDiv = DOM.create(
				"div",
				{
					style: {
						gridColumnStart: "1",
						gridColumnEnd: "4",
						textAlign: "center",
						fontSize: "0.875rem",
						fontStyle: "italic",
					},
				},
				"Hardware acceleration is ",
				DOM.create(
					"a",
					{
						href: "https://github.com/w3c/webcodecs/issues/896",
					},
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

customElements.define("moq-publish-support", MoqPublishSupport);

declare global {
	interface HTMLElementTagNameMap {
		"moq-publish-support": MoqPublishSupport;
	}
}
