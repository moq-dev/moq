// CSS styles for moq-boy web components, injected into shadow DOM.

export const cardStyles = `
	.card {
		position: relative; background: #111; border: 1px solid #333; border-radius: 8px;
		overflow: hidden; cursor: pointer; transition: all 0.3s ease;
		width: 320px; aspect-ratio: 10/9;
	}
	.card:hover { border-color: #8bac0f; }

	/* Expanded mode: fill the container */
	.card.expanded {
		width: 100%; height: 100%;
		border-radius: 0; border: none; aspect-ratio: auto;
		display: flex; flex-direction: row;
	}

	.card .video {
		width: 100%; height: 100%; object-fit: contain; background: #000; display: block;
		image-rendering: pixelated; outline: none;
	}
	.card .video:focus-visible {
		outline: 2px solid #8bac0f; outline-offset: -2px;
	}
	.card.expanded .video {
		flex: 1; min-width: 0;
	}

	.card .label {
		position: absolute; top: 0.5rem; right: 0.5rem;
		background: rgba(0,0,0,0.7); color: #8bac0f; padding: 0.2rem 0.5rem;
		border-radius: 4px; font-family: monospace; font-size: 0.7rem;
	}

	/* Location label in controls panel */
	.location {
		width: 100%; font-family: monospace; font-size: 0.65rem;
		color: #8bac0f; text-align: center; margin-bottom: 0.3rem;
	}

	/* Viewer latency list in controls panel */
	.latency-list {
		width: 100%; font-family: monospace; font-size: 0.65rem;
	}
	.latency-list .latency-header {
		color: #888; text-transform: uppercase; letter-spacing: 0.05em;
		margin-bottom: 0.3rem; font-size: 0.6rem;
	}
	.latency-entry {
		display: flex; justify-content: space-between;
		padding: 0.15rem 0; color: #aaa;
	}
	.latency-entry.self { color: #8bac0f; font-weight: 700; }
	.latency-note {
		font-family: monospace; font-size: 0.55rem; color: #555;
		text-align: center; line-height: 1.4; margin-top: 0.25rem;
	}

	/* Encoding stats list in controls panel */
	.stats-list {
		width: 100%; font-family: monospace; font-size: 0.65rem;
	}
	.stats-list .stats-header {
		color: #888; text-transform: uppercase; letter-spacing: 0.05em;
		margin-bottom: 0.3rem; font-size: 0.6rem;
	}
	.stats-entry {
		display: flex; justify-content: space-between;
		padding: 0.15rem 0; color: #aaa;
	}

	/* Controls panel */
	.card .controls { display: none; }
	.card.expanded .controls {
		display: flex; flex-direction: column; align-items: center; justify-content: center;
		width: 240px; background: #111; border-left: 1px solid #333;
		padding: 1rem; gap: 1rem; flex-shrink: 0;
	}

	.controls-inner {
		display: flex; flex-direction: column; align-items: center; gap: 1.5rem;
	}

	/* D-pad */
	.dpad {
		display: grid;
		grid-template-columns: 52px 52px 52px;
		grid-template-rows: 52px 52px 52px;
		gap: 4px;
	}
	.dpad-btn {
		background: #1a1a2e; color: #e0e0e0; border: 1px solid #555;
		border-radius: 6px; cursor: pointer; font-size: 1.2rem;
		display: flex; align-items: center; justify-content: center;
		transition: all 0.15s;
	}
	.dpad-btn:hover { background: #2a2a3e; border-color: #8bac0f; }
	.dpad-btn:active { background: #3a3a4e; transform: scale(0.95); }
	.dpad-up { grid-column: 2; grid-row: 1; }
	.dpad-left { grid-column: 1; grid-row: 2; }
	.dpad-right { grid-column: 3; grid-row: 2; }
	.dpad-down { grid-column: 2; grid-row: 3; }

	/* AB buttons */
	.ab-buttons { display: flex; gap: 0.75rem; }
	.ab-btn {
		width: 52px; height: 52px; border-radius: 50%;
		background: #2a1a3e; color: #c084fc; border: 2px solid #7c3aed;
		font-size: 1rem; font-weight: 700; cursor: pointer;
		display: flex; align-items: center; justify-content: center;
		transition: all 0.15s;
	}
	.ab-btn:hover { background: #3a2a4e; border-color: #a855f7; }
	.ab-btn:active { transform: scale(0.95); }

	/* Start/Select */
	.meta-buttons { display: flex; gap: 0.5rem; }
	.meta-btn {
		background: #222; color: #aaa; border: 1px solid #444;
		padding: 0.3rem 0.8rem; border-radius: 12px; cursor: pointer;
		font-size: 0.65rem; text-transform: uppercase; letter-spacing: 0.05em;
		transition: all 0.15s;
	}
	.meta-btn:hover { background: #333; border-color: #8bac0f; color: #e0e0e0; }
	.meta-btn:active { transform: scale(0.95); }

	/* Active button highlights (from remote status) */
	.dpad-btn.active { background: #1a3a1a; border-color: #8bac0f; color: #8bac0f; box-shadow: 0 0 8px rgba(139, 172, 15, 0.4); }
	.ab-btn.active { background: #3a1a4e; border-color: #a855f7; box-shadow: 0 0 8px rgba(168, 85, 247, 0.4); }
	.meta-btn.active { background: #2a2a1a; border-color: #facc15; color: #facc15; }

	/* Reset and audio */
	.util-buttons { display: flex; gap: 0.5rem; width: 100%; }
	.util-btn {
		flex: 1; background: #1a1a1a; color: #888; border: 1px solid #333;
		padding: 0.4rem; border-radius: 6px; cursor: pointer;
		font-size: 0.7rem; transition: all 0.15s;
	}
	.util-btn:hover { background: #2a2a2a; border-color: #666; color: #ccc; }
	.util-btn.unmuted { background: #1a2a1a; border-color: #8bac0f; color: #8bac0f; }
	.util-btn.reset { color: #f87171; border-color: #7f1d1d; }
	.util-btn.reset:hover { background: #2a1a1a; border-color: #f87171; }

	.jitter-container {
		width: 100%; display: flex; flex-direction: column; align-items: center; gap: 0.3rem;
	}
	.jitter-label {
		font-family: monospace; font-size: 0.65rem; color: #888;
	}
	.jitter-slider {
		width: 100%; accent-color: #8bac0f; cursor: pointer;
	}

	.key-hints {
		font-family: monospace; font-size: 0.6rem; color: #555;
		text-align: center; line-height: 1.6;
	}
`;

export const gridStyles = `
	* { margin: 0; padding: 0; box-sizing: border-box; }
	:host, body { display: flex; flex-direction: column; font-family: system-ui, sans-serif; background: #0a0a0a; color: #e0e0e0; min-height: 100vh; }

	header {
		background: #111; border-bottom: 1px solid #333;
		padding: 0.75rem 1.5rem; display: flex; align-items: center; gap: 1rem;
	}
	header h1 { font-size: 1.1rem; font-weight: 600; color: #8bac0f; }
	header .status { font-size: 0.75rem; margin-left: auto; }

	.about {
		max-width: 500px; margin: 1.5rem auto; padding: 0 1.5rem;
		font-size: 0.75rem; color: #555; line-height: 1.7;
	}
	.about p { margin-bottom: 0.5rem; }
	.about a { color: #8bac0f; text-decoration: none; }
	.about a:hover { text-decoration: underline; }
	.about ul { list-style: none; padding: 0; margin: 0; }
	.about li { padding-left: 1rem; position: relative; }
	.about li::before { content: "\\2022"; color: #8bac0f; position: absolute; left: 0; }

	.grid {
		display: flex; flex-wrap: wrap; gap: 1rem; padding: 1rem;
	}
	.grid:has(.card.expanded) {
		padding: 0; gap: 0; flex: 1;
	}
	.grid:has(.card.expanded) .card:not(.expanded) {
		display: none;
	}
	:host:has(.card.expanded) header,
	:host:has(.card.expanded) .about,
	body:has(.card.expanded) > header,
	body:has(.card.expanded) > .about {
		display: none;
	}

	.empty-state {
		padding: 4rem 2rem; text-align: center; width: 100%;
	}
	.empty-state .icon { font-size: 3rem; margin-bottom: 1rem; opacity: 0.3; }
	.empty-state .msg { font-size: 1rem; color: #666; }
	.empty-state .hint { font-size: 0.75rem; color: #444; margin-top: 0.5rem; }

	${cardStyles}
`;

export const previewStyles = `
	:host {
		display: block; position: relative; overflow: hidden;
		background: #000; cursor: pointer;
	}

	canvas {
		width: 100%; height: 100%; object-fit: contain; display: block;
		image-rendering: pixelated;
	}

	.label {
		position: absolute; top: 0.5rem; right: 0.5rem;
		background: rgba(0,0,0,0.7); color: #8bac0f; padding: 0.2rem 0.5rem;
		border-radius: 4px; font-family: monospace; font-size: 0.7rem;
	}
`;
