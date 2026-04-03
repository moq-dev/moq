import { GameCard, Moq } from "@moq/boy";
import { gridStyles } from "@moq/boy/styles";

// Inject styles into the document.
const style = document.createElement("style");
style.textContent = gridStyles;
document.head.appendChild(style);

// Header.
const header = document.createElement("header");
const h1 = document.createElement("h1");
h1.textContent = "MoQ Boy";
const statusEl = document.createElement("span");
statusEl.className = "status";
statusEl.textContent = "Disconnected";
header.appendChild(h1);
header.appendChild(statusEl);
document.body.appendChild(header);

// Grid.
const gridEl = document.createElement("div");
gridEl.className = "grid";
document.body.appendChild(gridEl);

// Empty state.
const emptyState = document.createElement("div");
emptyState.className = "empty-state";

const emptyIcon = document.createElement("div");
emptyIcon.className = "icon";
emptyIcon.textContent = "\u{1F3AE}";
emptyState.appendChild(emptyIcon);

const emptyMsg = document.createElement("div");
emptyMsg.className = "msg";
emptyMsg.textContent = "No games online";
emptyState.appendChild(emptyMsg);

const emptyHint = document.createElement("div");
emptyHint.className = "hint";
emptyHint.textContent = "Waiting for Game Boy sessions to connect...";
emptyState.appendChild(emptyHint);

gridEl.appendChild(emptyState);

// About section.
const about = document.createElement("div");
about.className = "about";

const aboutP1 = document.createElement("p");
aboutP1.textContent = "Click a game to play. Everyone controls the same game (anarchy mode).";
about.appendChild(aboutP1);

const aboutP2 = document.createElement("p");
aboutP2.textContent = "A generic ";
const moqLink = document.createElement("a");
moqLink.href = "https://moq.dev";
moqLink.textContent = "MoQ";
aboutP2.appendChild(moqLink);
aboutP2.appendChild(document.createTextNode(" relay is used for everything:"));
about.appendChild(aboutP2);

const aboutUl = document.createElement("ul");
for (const text of [
	"Discovering online games and players.",
	"Transmitting audio/video tracks, metadata, and (multiple) player controls.",
	"Subscribing to audio/video on-demand.",
	"Pausing emulation/encoding when there are no subscribers.",
]) {
	const li = document.createElement("li");
	li.textContent = text;
	aboutUl.appendChild(li);
}
about.appendChild(aboutUl);
document.body.appendChild(about);

// Connection.
const url = import.meta.env.VITE_RELAY_URL || "http://localhost:4443/anon";
const enabled = new Moq.Signals.Signal(true);
const connection = new Moq.Connection.Reload({ url: new URL(url), enabled });

const signals = new Moq.Signals.Effect();
const sessions = new Map<string, GameCard>();
const expanded = new Moq.Signals.Signal<string | undefined>(undefined);

function updateEmptyState() {
	emptyState.style.display = sessions.size === 0 ? "block" : "none";
}

// Track connection status.
signals.run((e) => {
	const status = e.get(connection.status);
	statusEl.textContent = status.charAt(0).toUpperCase() + status.slice(1);
	statusEl.style.color = status === "connected" ? "#8bac0f" : status === "connecting" ? "#facc15" : "#888";
});

// Discover game sessions via announcements.
signals.run((effect) => {
	const conn = effect.get(connection.established);
	if (!conn) return;

	const announced = conn.announced(Moq.Path.from("boy"));
	effect.cleanup(() => announced.close());

	effect.spawn(async () => {
		for (;;) {
			const entry = await Promise.race([effect.cancel, announced.next()]);
			if (!entry) break;

			const suffix = Moq.Path.stripPrefix(Moq.Path.from("boy"), entry.path);
			if (!suffix || suffix.includes("/")) continue;

			const id = suffix;
			if (entry.active && !sessions.has(id)) {
				const card = new GameCard({
					sessionId: id,
					connection,
					expanded,
					root: document.body,
				});
				sessions.set(id, card);
				gridEl.appendChild(card.el);
				updateEmptyState();
			} else if (!entry.active) {
				const card = sessions.get(id);
				if (card) {
					card.close();
					card.el.remove();
					sessions.delete(id);
					updateEmptyState();
				}
			}
		}
	});
});
