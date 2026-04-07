import { For, Show } from "solid-js";
import { useGameUI } from "../hooks/use-boy-ui";

/** Right-side panel showing location, encoding stats, and per-viewer latency. */
export default function StatsPanel() {
	const ctx = useGameUI();

	const location = () => ctx.status()?.location;
	const stats = () => ctx.status()?.stats;

	const latencyEntries = () => {
		const lat = ctx.status()?.latency ?? {};
		return Object.entries(lat);
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
				<div class="boy__location">{location()}</div>
			</Show>

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

			<div class="boy__stats-note">Emulation and encoding are paused when there are no viewers.</div>

			<Show when={latencyEntries().length > 0}>
				<div class="boy__latency-list">
					<div class="boy__latency-header">Players ({latencyEntries().length})</div>
					<For each={latencyEntries()}>
						{([id, ms]) => (
							<div
								class="boy__latency-entry"
								classList={{ "boy__latency-entry--self": id === ctx.viewerId() }}
							>
								<span>{id === ctx.viewerId() ? `${id} (you)` : id}</span>
								<span>{ms}ms</span>
							</div>
						)}
					</For>
				</div>
				<div class="boy__stats-note">Includes both the render delay AND the input delay.</div>
			</Show>
		</div>
	);
}
