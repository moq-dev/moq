import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

/** Shows why the relay refused the connection, or the origin the broadcast, in place of the offline notice. */
export function refusedIndicator(parent: Effect, watch: MoqWatch): HTMLElement {
	const container = document.createElement("div");
	container.className = "watch-ui__notice watch-ui__refused-indicator";
	container.setAttribute("role", "status");
	container.setAttribute("aria-live", "polite");

	const text = document.createElement("span");
	text.className = "watch-ui__notice-text";
	container.appendChild(text);

	parent.run((effect) => {
		// A connection that gave up, as on a refused token, explains more than an older
		// refusal of the broadcast, which no session is asking about any more.
		const connection = effect.get(watch.connection.error);
		const broadcast = effect.get(watch.broadcast.out.error);
		container.style.display = connection || broadcast ? "" : "none";
		if (connection) {
			text.textContent = connection.message ? `Could not connect: ${connection.message}` : "Could not connect";
		} else if (broadcast) {
			text.textContent = broadcast.message
				? `This broadcast was refused: ${broadcast.message}`
				: "This broadcast was refused";
		}
	});

	return container;
}
