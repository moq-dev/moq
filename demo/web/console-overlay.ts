import type { Plugin } from "vite";

// Dev-only overlay that mirrors console.warn/console.error (plus uncaught
// errors and unhandled rejections) onto the page. The demos are often driven
// by an AI that can read the DOM but not the browser console, so surfacing
// failures visually is the only way for it to see what broke.
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
const container = document.createElement("div");
container.id = "moq-console-overlay";
Object.assign(container.style, {
	position: "fixed",
	bottom: "0",
	left: "0",
	right: "0",
	maxHeight: "40vh",
	overflowY: "auto",
	zIndex: "2147483647",
	font: "12px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace",
	background: "rgba(0, 0, 0, 0.85)",
	color: "#eee",
	borderTop: "1px solid #444",
	display: "none",
	pointerEvents: "auto",
});

function append(level, args) {
	const line = document.createElement("div");
	Object.assign(line.style, {
		padding: "2px 8px",
		borderBottom: "1px solid #222",
		whiteSpace: "pre-wrap",
		wordBreak: "break-word",
		color: level === "error" ? "#ff8080" : "#ffd480",
	});
	const text = args
		.map((a) => {
			if (a instanceof Error) return a.stack || a.message;
			if (typeof a === "string") return a;
			try {
				return JSON.stringify(a);
			} catch {
				return String(a);
			}
		})
		.join(" ");
	line.textContent = "[" + level + "] " + text;
	container.appendChild(line);
	container.style.display = "block";
	container.scrollTop = container.scrollHeight;
}

function install() {
	if (!document.body) {
		document.addEventListener("DOMContentLoaded", install, { once: true });
		return;
	}
	document.body.appendChild(container);
}
install();

for (const level of ["warn", "error"]) {
	const original = console[level].bind(console);
	console[level] = (...args) => {
		original(...args);
		append(level, args);
	};
}

window.addEventListener("error", (e) => {
	append("error", [e.error ?? e.message]);
});
window.addEventListener("unhandledrejection", (e) => {
	append("error", ["Unhandled rejection:", e.reason]);
});
`;
