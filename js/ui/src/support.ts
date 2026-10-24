import { Effect, Signal } from "@moq/signals";
import * as DOM from "@moq/signals/dom";

/** How well a feature works: natively, through a fallback, or not at all. */
export type Level = "full" | "partial" | "none";

/**
 * One feature's support, rendered by its type:
 * - a boolean: yes or no.
 * - a {@link Level}: full, polyfill, or none.
 * - a codec: hardware, software, or no; `hardware` is undefined when it can't be detected.
 * - undefined: no.
 */
export type Status = boolean | Level | { hardware?: boolean; software: boolean } | undefined;

/** What a {@link Support} element renders: an overall summary and a row per feature. */
export interface Report {
	/** The overall level, shown in the banner and compared against `show`. */
	summary: Level;
	/** Each row's group label (blank continues the group above), feature name, and status. */
	rows: [group: string, feature: string, status: Status][];
}

/**
 * Whether to display the support banner.
 * - "always": Always display the banner.
 * - "warning": Display the banner if a required feature needs a polyfill/fallback.
 * - "error": Display the banner if a required feature is unsupported.
 * - "never": Never display the banner.
 */
export type Show = "always" | "warning" | "error" | "never";

const OBSERVED = ["show", "details"] as const;
type Observed = (typeof OBSERVED)[number];

/**
 * A banner reporting what this browser supports, with an optional details table.
 *
 * Subclass it with {@link Support.detect} and register the subclass as a custom element.
 */
export abstract class Support extends HTMLElement {
	#show = new Signal<Show>("warning");
	#details = new Signal<boolean>(false);
	#report = new Signal<Report | undefined>(undefined);
	#close = new Signal<boolean>(false);

	#signals?: Effect;

	static observedAttributes = OBSERVED;

	constructor() {
		super();

		this.detect()
			.then((report) => this.#report.set(report))
			.catch((err) => console.error(`Failed to detect ${this.localName}:`, err));
	}

	/** Detect what this browser supports, called once on construction. */
	protected abstract detect(): Promise<Report>;

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

	/** When to display the banner, mirrored by the `show` attribute. */
	get show(): Show {
		return this.#show.peek();
	}

	set show(show: Show) {
		this.#show.set(show);
	}

	/** Whether to list each feature, mirrored by the `details` attribute. */
	get details(): boolean {
		return this.#details.peek();
	}

	set details(details: boolean) {
		this.#details.set(details);
	}

	#render(effect: Effect) {
		const report = effect.get(this.#report);
		if (!report) return;

		const close = effect.get(this.#close);
		if (close) return;

		const show = effect.get(this.#show);
		if (show === "never") return;

		// Don't render the banner if we have full support and they only asked for warnings.
		if (show === "warning" && report.summary === "full") return;

		// Don't render the banner if we have at least partial support and they only asked for errors.
		if (show === "error" && report.summary !== "none") return;

		const container = DOM.create("div", {
			style: {
				margin: "0 auto",
				maxWidth: "28rem",
				padding: "1rem",
			},
		});

		this.appendChild(container);
		effect.cleanup(() => this.removeChild(container));

		this.#renderHeader(container, report.summary, effect);

		if (effect.get(this.#details)) {
			this.#renderDetails(container, report, effect);
		}
	}

	#renderHeader(parent: HTMLDivElement, summary: Level, effect: Effect) {
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

	#renderDetails(parent: HTMLDivElement, report: Report, effect: Effect) {
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

		// Whether any codec reported software support without a detectable hardware answer.
		let undetectable = false;

		const format = (status: Status): string => {
			if (typeof status === "object") {
				if (status.hardware) return "🟢 Hardware";
				if (!status.software) return "🔴 No";
				if (status.hardware !== undefined) return "🟡 Software";
				undetectable = true;
				return "🟡 Software*";
			}
			if (status === "full") return "🟢 Full";
			if (status === "partial") return "🟡 Polyfill";
			if (status === "none") return "🔴 None";
			return status ? "🟢 Yes" : "🔴 No";
		};

		for (const [group, feature, status] of report.rows) {
			container.append(
				DOM.create("div", { style: { gridColumnStart: "1", fontWeight: "bold", textAlign: "right" } }, group),
				DOM.create("div", { style: { gridColumnStart: "2", textAlign: "center" } }, feature),
				DOM.create("div", { style: { gridColumnStart: "3" } }, format(status)),
			);
		}

		if (undetectable) {
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
				"*Hardware acceleration is ",
				DOM.create(
					"a",
					{
						href: "https://github.com/w3c/webcodecs/issues/896",
					},
					"undetectable",
				),
				" in this browser.",
			);
			container.appendChild(noteDiv);
		}

		parent.appendChild(container);
		effect.cleanup(() => parent.removeChild(container));
	}
}
