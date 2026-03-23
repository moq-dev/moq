import * as Moq from "@moq/lite";

/**
 * Robot grid: discovers robots via announcement prefix and renders a clickable card per robot.
 */
export class RobotGrid {
	element: HTMLDivElement;
	#signals = new Moq.Signals.Effect();
	#cards = new Map<string, HTMLDivElement>();

	constructor(connection: Moq.Connection.Reload, onSelect: (robotId: string) => void) {
		this.element = document.createElement("div");
		this.element.style.cssText = `
			display: grid;
			grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
			gap: 1.5rem;
			padding: 1rem 0;
		`;

		const emptyMsg = document.createElement("div");
		emptyMsg.textContent = "Waiting for robots to come online...";
		emptyMsg.style.cssText = "color: #666; font-style: italic; grid-column: 1 / -1;";
		this.element.appendChild(emptyMsg);

		// Reactively watch for announcements.
		this.#signals.run((effect) => {
			const conn = effect.get(connection.established);
			if (!conn) return;

			const announced = conn.announced(Moq.Path.from("robot"));
			effect.cleanup(() => announced.close());

			effect.spawn(async () => {
				for (;;) {
					const entry = await Promise.race([effect.cancel, announced.next()]);
					if (!entry) break;

					// Filter to single-component suffixes (robot IDs, not nested paths like viewer/).
					const suffix = Moq.Path.stripPrefix(Moq.Path.from("robot"), entry.path);
					if (!suffix || suffix.includes("/")) continue;

					const robotId = suffix;

					if (entry.active) {
						// Remove empty message on first robot.
						if (emptyMsg.parentNode) emptyMsg.remove();

						if (!this.#cards.has(robotId)) {
							const card = this.#createCard(robotId, onSelect);
							this.#cards.set(robotId, card);
							this.element.appendChild(card);
						}
					} else {
						const card = this.#cards.get(robotId);
						if (card) {
							card.remove();
							this.#cards.delete(robotId);
						}
						if (this.#cards.size === 0 && !emptyMsg.parentNode) {
							this.element.appendChild(emptyMsg);
						}
					}
				}
			});
		});
	}

	#createCard(robotId: string, onSelect: (id: string) => void): HTMLDivElement {
		const card = document.createElement("div");
		card.style.cssText = `
			background: #1a1a1a;
			border: 1px solid #333;
			border-radius: 8px;
			padding: 1.5rem;
			cursor: pointer;
			transition: border-color 0.2s, background 0.2s;
		`;

		card.addEventListener("mouseenter", () => {
			card.style.borderColor = "#4ade80";
			card.style.background = "#1a2e1a";
		});
		card.addEventListener("mouseleave", () => {
			card.style.borderColor = "#333";
			card.style.background = "#1a1a1a";
		});

		const title = document.createElement("h3");
		title.textContent = robotId;
		title.style.cssText = "color: #4ade80; font-family: monospace; font-size: 1.1rem; margin-bottom: 0.5rem;";
		card.appendChild(title);

		const status = document.createElement("div");
		status.textContent = "Online";
		status.style.cssText = "color: #4ade80; font-size: 0.85rem;";
		card.appendChild(status);

		card.addEventListener("click", () => onSelect(robotId));
		return card;
	}

	close() {
		this.#signals.close();
	}
}
