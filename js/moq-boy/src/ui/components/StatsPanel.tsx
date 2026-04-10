import { createAccessor } from "@moq/signals/solid";
import { For, Show } from "solid-js";
import { useGameUI } from "../hooks/use-boy-ui";

/** Right-side panel showing location, encoding stats, buffer slider, and per-viewer latency. */
export default function StatsPanel() {
	const ctx = useGameUI();
	const game = ctx.game;

	const jitter = createAccessor(game.sync.jitter);

	const onJitterInput = (e: Event) => {
		const el = e.currentTarget as HTMLInputElement;
		game.latency.set(Number.parseInt(el.value, 10) as import("@moq/lite").Time.Milli);
	};

	const location = () => ctx.status()?.location;
	const stats = () => ctx.status()?.stats;

	const playerCount = () => Object.keys(ctx.status()?.latency ?? {}).length;

	const latencyEntries = () => {
		const id = ctx.viewerId();
		if (!id) return [];
		return ctx.status()?.latency?.[id] ?? [];
	};

	const pct = (value: number) => {
		const wall = stats()?.wall_secs ?? 0;
		return wall > 0 ? Math.round((value / wall) * 100) : 0;
	};

	const statsItems = () => {
		const s = stats();
		if (!s) return [];
		return [
			{ label: "Emulation", secs: s.emulation_secs },
			{ label: "Video", secs: s.video_secs },
			{ label: "Audio", secs: s.audio_secs },
		];
	};

	return (
		<div class="boy__stats">
			<Show when={location()}>
				<div class="boy__location">
					<div class="boy__location-label">Emulator Location</div>
					<div class="boy__location-value">{location()}</div>
				</div>
			</Show>

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

			<Show when={stats()}>
				<div class="boy__stats-list">
					<div class="boy__stats-header">Uptime ({stats()?.wall_secs}s)</div>
					<For each={statsItems()}>
						{(item) => (
							<div class="boy__stats-entry">
								<span>{item.label}</span>
								<span>
									{item.secs}s ({pct(item.secs)}%)
								</span>
							</div>
						)}
					</For>
				</div>
			</Show>

			<div class="boy__stats-note">
				Emulation and encoding are paused when there are no viewers. Try muting or tabbing away!
			</div>

			<Show when={playerCount() > 0}>
				<div class="boy__latency-list">
					<div class="boy__latency-header">Latency ({playerCount()} players)</div>
					<For each={latencyEntries()}>
						{(entry) => (
							<div class="boy__latency-entry">
								<span>{entry.label}</span>
								<span>{entry.ms}ms</span>
							</div>
						)}
					</For>
					<Show when={latencyEntries().length === 0}>
						<div class="boy__stats-note">Press a button to see your latency breakdown.</div>
					</Show>
				</div>
			</Show>
		</div>
	);
}
