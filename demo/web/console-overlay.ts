import type { Plugin } from "vite";

// Dev-only logger that mirrors the browser console (plus uncaught errors and
// unhandled rejections) onto the page. The demos are often driven by an AI that
// can read the DOM but not the browser console, so surfacing output visually is
// the only way for it to see what happened.
//
// The injected script defines a <moq-console> custom element. Drop it anywhere
// in a page to render the log inline at that spot. Set `level` to choose the
// minimum severity shown (debug | log | info | warn | error; defaults to warn).
// If a page doesn't include one, a floating instance is pinned to the bottom as
// a fallback, so capture is automatic everywhere.
export function consoleOverlay(): Plugin {
	return {
		name: "moq-console-overlay",
		apply: "serve",
		transformIndexHtml() {
			return [
				{
					tag: "script",
					attrs: { type: "module" },
					children: OVERLAY_SCRIPT,
					injectTo: "head",
				},
			];
		},
	};
}

const OVERLAY_SCRIPT = `
const LEVELS = ["debug", "log", "info", "warn", "error"];
const COLORS = {
	debug: "#999",
	log: "#ccc",
	info: "#80c0ff",
	warn: "#ffd480",
	error: "#ff8080",
};
// Max int32: keep the floating overlay above everything else on the page.
const MAX_Z_INDEX = 2147483647;

// Buffer messages from the moment this module loads, before any element exists,
// so an inline <moq-console> added later still shows earlier messages. We patch
// every level and let each element filter by its own threshold. The buffer is
// capped so a noisy long-running demo can't grow it without bound.
const BUFFER_MAX = 1000;
const buffer = [];
const sinks = new Set();

function format(args) {
	return args
		.map((a) => {
			if (a instanceof Error) return a.stack || a.message;
			if (typeof a === "string") return a;
			if (a === undefined) return "undefined";
			try {
				return JSON.stringify(a) ?? String(a);
			} catch {
				return String(a);
			}
		})
		.join(" ");
}

function emit(level, args) {
	const entry = { level, text: format(args) };
	buffer.push(entry);
	if (buffer.length > BUFFER_MAX) buffer.shift();
	// Never let a misbehaving sink throw out of the patched console method and
	// break the app's own logging call site.
	for (const sink of sinks) {
		try {
			sink(entry);
		} catch {}
	}
}

for (const level of LEVELS) {
	const original = console[level].bind(console);
	console[level] = (...args) => {
		original(...args);
		emit(level, args);
	};
}
window.addEventListener("error", (e) => emit("error", [e.error ?? e.message]));
window.addEventListener("unhandledrejection", (e) => emit("error", ["Unhandled rejection:", e.reason]));

class MoqConsole extends HTMLElement {
	static observedAttributes = ["level"];

	connectedCallback() {
		const floating = this.hasAttribute("floating");
		Object.assign(this.style, {
			display: "none",
			flexDirection: "column",
			maxHeight: floating ? "40vh" : "30vh",
			overflow: "hidden",
			font: "12px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace",
			background: "rgba(0, 0, 0, 0.85)",
			color: "#eee",
			border: "1px solid #444",
			borderRadius: "4px",
		});
		if (floating) {
			Object.assign(this.style, {
				position: "fixed",
				bottom: "0",
				left: "0",
				right: "0",
				borderRadius: "0",
				borderLeftWidth: "0",
				borderRightWidth: "0",
				borderBottomWidth: "0",
				zIndex: String(MAX_Z_INDEX),
			});
		}

		// Header bar with the level dropdown pushed to the far right.
		const header = document.createElement("div");
		Object.assign(header.style, {
			display: "flex",
			alignItems: "center",
			justifyContent: "space-between",
			gap: "8px",
			padding: "2px 8px",
			borderBottom: "1px solid #333",
			color: "#888",
			flex: "0 0 auto",
		});
		const label = document.createElement("span");
		label.textContent = "console";
		const select = document.createElement("select");
		Object.assign(select.style, {
			font: "inherit",
			background: "#222",
			color: "#eee",
			border: "1px solid #444",
			borderRadius: "3px",
			padding: "0 2px",
		});
		for (const level of LEVELS) {
			const option = document.createElement("option");
			option.value = level;
			option.textContent = level;
			select.appendChild(option);
		}
		select.value = this.levelName();
		select.addEventListener("change", () => this.setAttribute("level", select.value));
		this.select = select;
		header.append(label, select);

		this.body = document.createElement("div");
		Object.assign(this.body.style, { overflowY: "auto", flex: "1 1 auto" });

		this.replaceChildren(header, this.body);

		this.sink = (entry) => {
			if (this.shows(entry.level)) this.append(entry);
		};
		sinks.add(this.sink);
		this.render();
	}

	disconnectedCallback() {
		sinks.delete(this.sink);
	}

	attributeChangedCallback() {
		// Ignore the initial attribute parse; connectedCallback builds the DOM.
		if (this.body) this.render();
	}

	levelName() {
		const name = this.getAttribute("level");
		return LEVELS.includes(name) ? name : "warn";
	}

	threshold() {
		return LEVELS.indexOf(this.levelName());
	}

	shows(level) {
		return LEVELS.indexOf(level) >= this.threshold();
	}

	render() {
		if (this.select) this.select.value = this.levelName();
		this.body.replaceChildren();
		for (const entry of buffer) {
			if (this.shows(entry.level)) this.append(entry);
		}
		this.updateVisibility();
	}

	updateVisibility() {
		// A floating fallback stays out of the way until there's something to show;
		// an explicitly placed inline logger always shows so its dropdown is usable.
		const hide = this.hasAttribute("floating") && this.body.childElementCount === 0;
		this.style.display = hide ? "none" : "flex";
	}

	append(entry) {
		const line = document.createElement("div");
		Object.assign(line.style, {
			padding: "2px 8px",
			borderTop: this.body.childElementCount ? "1px solid #222" : "none",
			whiteSpace: "pre-wrap",
			wordBreak: "break-word",
			color: COLORS[entry.level] ?? "#eee",
		});
		line.textContent = "[" + entry.level + "] " + entry.text;
		this.body.appendChild(line);
		this.updateVisibility();
		this.body.scrollTop = this.body.scrollHeight;
	}
}
customElements.define("moq-console", MoqConsole);

// Fallback: if a page didn't place an inline logger, pin a floating one so
// output still surfaces automatically.
window.addEventListener("DOMContentLoaded", () => {
	if (document.querySelector("moq-console")) return;
	const el = document.createElement("moq-console");
	el.setAttribute("floating", "");
	document.body.appendChild(el);
});
`;
