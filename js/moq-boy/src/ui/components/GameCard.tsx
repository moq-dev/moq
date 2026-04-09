import { Signals } from "@moq/lite";
import { createEffect, onCleanup, onMount, Show } from "solid-js";
import type { Game } from "../../index.ts";
import { KEY_MAP } from "../../index.ts";
import { GameUIContextProvider } from "../context";
import { useGameUI } from "../hooks/use-boy-ui";
import Controls from "./Controls";
import StatsPanel from "./StatsPanel";

export default function GameCard(props: { game: Game }) {
	return (
		<GameUIContextProvider game={props.game}>
			<GameCardInner />
		</GameUIContextProvider>
	);
}

function GameCardInner() {
	const ctx = useGameUI();
	const game = ctx.game;

	let canvasRef!: HTMLCanvasElement;
	const signals = new Signals.Effect();

	// Set canvas on the video renderer once mounted.
	onMount(() => {
		game.videoRenderer.canvas.set(canvasRef);
	});

	// Keyboard input — preventDefault when expanded or hovered.
	const isActive = () => game.expanded.peek() === game.sessionId || game.hovered.peek();

	const onKeyDown = (e: KeyboardEvent) => {
		if (!isActive()) return;
		if (e.repeat) return;

		const button = KEY_MAP[e.key];
		if (button) {
			game.heldButtons.add(button);
			game.sendButtons();
			e.preventDefault();
		} else if (e.key === "Escape" && ctx.expanded()) {
			game.expanded.set(undefined);
			e.preventDefault();
		}
	};

	const onKeyUp = (e: KeyboardEvent) => {
		if (!isActive()) return;
		const button = KEY_MAP[e.key];
		if (button) {
			game.heldButtons.delete(button);
			game.sendButtons();
			e.preventDefault();
		}
	};

	const onBlur = () => {
		if (game.heldButtons.size > 0) {
			game.heldButtons.clear();
			game.sendButtons();
		}
	};

	// Clear buttons when card becomes inactive.
	createEffect(() => {
		if (!ctx.active() && game.heldButtons.size > 0) {
			game.heldButtons.clear();
			game.sendButtons();
		}
	});

	// Register global keyboard listeners.
	signals.event(document, "keydown", onKeyDown);
	signals.event(document, "keyup", onKeyUp);
	signals.event(window, "blur", onBlur);

	onCleanup(() => signals.close());

	// Label: session name + player count.
	const label = () => {
		const status = ctx.status();
		const n = status ? Object.keys(status.latency).length : 0;
		return n > 0 ? `${game.sessionId} (${n})` : game.sessionId;
	};

	return (
		// biome-ignore lint/a11y/noStaticElementInteractions: mouse events for hover tracking, not interaction
		<div
			class="boy__card"
			classList={{ "boy__card--expanded": ctx.expanded() }}
			onMouseEnter={() => game.hovered.set(true)}
			onMouseLeave={() => game.hovered.set(false)}
		>
			<canvas
				ref={canvasRef}
				class="boy__video"
				tabIndex={0}
				onClick={() => game.expanded.set(game.sessionId)}
				onKeyDown={(e) => {
					if (e.key === " ") {
						e.preventDefault();
						e.stopPropagation();
						game.expanded.set(game.sessionId);
					}
				}}
			/>
			<div class="boy__label">{label()}</div>
			<Show when={ctx.expanded()}>
				<div class="boy__panel boy__panel--left">
					<Controls />
				</div>
				<div class="boy__panel boy__panel--right">
					<StatsPanel />
				</div>
			</Show>
		</div>
	);
}
