import * as Moq from "@moq/lite";
import { RobotGrid } from "./grid";
import { RobotViewer } from "./viewer";

// Parse URL params.
const params = new URLSearchParams(window.location.search);
const relayUrl = params.get("url") ?? "https://relay.quic.video/anon";

const statusEl = document.getElementById("connection-status")!;
const appEl = document.getElementById("app")!;

// Set up connection with auto-reconnect.
const connection = new Moq.Connection.Reload({
	url: new URL(relayUrl),
	enabled: true,
});

// Track connection status.
const effect = new Moq.Signals.Effect();
effect.run((e) => {
	const status = e.get(connection.status);
	statusEl.textContent = status.charAt(0).toUpperCase() + status.slice(1);
	statusEl.style.color = status === "connected" ? "#4ade80" : status === "connecting" ? "#facc15" : "#888";
});

// If a robot ID is specified, go directly to viewer mode.
const robotId = params.get("robot");
if (robotId) {
	const viewer = new RobotViewer(connection, robotId, relayUrl);
	appEl.appendChild(viewer.element);
} else {
	const grid = new RobotGrid(connection, (id: string) => {
		// Navigate to viewer for this robot.
		const url = new URL(window.location.href);
		url.searchParams.set("robot", id);
		window.history.pushState({}, "", url);
		// Clear app container safely.
		while (appEl.firstChild) appEl.removeChild(appEl.firstChild);
		const viewer = new RobotViewer(connection, id, relayUrl);
		appEl.appendChild(viewer.element);
	});
	appEl.appendChild(grid.element);
}
