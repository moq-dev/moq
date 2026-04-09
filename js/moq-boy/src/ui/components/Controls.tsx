import { createAccessor, createPair } from "@moq/signals/solid";
import { For } from "solid-js";
import { useGameUI } from "../hooks/use-boy-ui";

/** All game controls: D-pad, A/B, Start/Select, Mute, Reset, Jitter slider, Key hints. */
export default function Controls() {
	const ctx = useGameUI();
	const game = ctx.game;

	const jitter = createAccessor(game.sync.jitter);
	const [userMuted, setUserMuted] = createPair(game.userMuted);

	const toggleMute = (e: MouseEvent) => {
		e.stopPropagation();
		setUserMuted((prev) => !prev);
	};

	const onReset = (e: MouseEvent) => {
		e.stopPropagation();
		game.sendCommand({ type: "reset" });
	};

	const onJitterInput = (e: Event) => {
		const el = e.currentTarget as HTMLInputElement;
		game.jitter.set(Number.parseInt(el.value, 10) as import("@moq/lite").Time.Milli);
	};

	return (
		<div class="boy__controls">
			<Dpad />
			<ABButtons />
			<MetaButtons />

			<div class="boy__util-buttons">
				<button
					type="button"
					class="boy__util-btn"
					classList={{ "boy__util-btn--unmuted": !userMuted() }}
					onClick={toggleMute}
				>
					{userMuted() ? "Unmute" : "Mute"}
				</button>
				<button type="button" class="boy__util-btn boy__util-btn--reset" onClick={onReset}>
					Reset
				</button>
			</div>

			<label class="boy__jitter">
				<span class="boy__jitter-label">Buffer: {jitter()}ms</span>
				<input
					type="range"
					class="boy__jitter-slider"
					min="0"
					max="500"
					value={jitter()}
					onInput={onJitterInput}
					onClick={(e) => e.stopPropagation()}
				/>
			</label>

			<div class="boy__key-hints">
				<div>Arrows: D-pad</div>
				<div>Z: B &nbsp; X: A</div>
				<div>Enter: Start &nbsp; Shift: Select</div>
				<div>Esc: Collapse</div>
			</div>
		</div>
	);
}

/** D-pad: up, down, left, right arranged in a grid. */
function Dpad() {
	const buttons = [
		{ className: "boy__dpad-up", label: "\u25B2", name: "up" },
		{ className: "boy__dpad-left", label: "\u25C4", name: "left" },
		{ className: "boy__dpad-right", label: "\u25BA", name: "right" },
		{ className: "boy__dpad-down", label: "\u25BC", name: "down" },
	];

	return (
		<div class="boy__dpad">
			<For each={buttons}>
				{(btn) => <GameButton className={`boy__dpad-btn ${btn.className}`} label={btn.label} name={btn.name} />}
			</For>
		</div>
	);
}

/** A and B buttons. */
function ABButtons() {
	return (
		<div class="boy__ab-buttons">
			<GameButton className="boy__ab-btn" label="B" name="b" />
			<GameButton className="boy__ab-btn" label="A" name="a" />
		</div>
	);
}

/** Start and Select buttons. */
function MetaButtons() {
	return (
		<div class="boy__meta-buttons">
			<GameButton className="boy__meta-btn" label="Select" name="select" />
			<GameButton className="boy__meta-btn" label="Start" name="start" />
		</div>
	);
}

/**
 * A single game button with mouse/touch input and active highlight.
 * The "active" state comes from the server status track (any player pressing).
 */
function GameButton(props: { className: string; label: string; name: string }) {
	const ctx = useGameUI();
	const game = ctx.game;

	const isActive = () => {
		const status = ctx.status();
		return status?.buttons.includes(props.name) ?? false;
	};

	const onMouseDown = (e: MouseEvent) => {
		e.stopPropagation();
		game.heldButtons.add(props.name);
		game.sendButtons();
	};

	const onMouseUp = (e: MouseEvent) => {
		e.stopPropagation();
		game.heldButtons.delete(props.name);
		game.sendButtons();
	};

	const onMouseLeave = () => {
		if (game.heldButtons.has(props.name)) {
			game.heldButtons.delete(props.name);
			game.sendButtons();
		}
	};

	const onTouchStart = (e: TouchEvent) => {
		e.preventDefault();
		game.heldButtons.add(props.name);
		game.sendButtons();
	};

	const onTouchEnd = (e: TouchEvent) => {
		e.preventDefault();
		game.heldButtons.delete(props.name);
		game.sendButtons();
	};

	return (
		<button
			type="button"
			class={props.className}
			classList={{ "boy__btn--active": isActive() }}
			data-button={props.name}
			onMouseDown={onMouseDown}
			onMouseUp={onMouseUp}
			onMouseLeave={onMouseLeave}
			onTouchStart={onTouchStart}
			onTouchEnd={onTouchEnd}
		>
			{props.label}
		</button>
	);
}
