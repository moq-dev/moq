import type { Effect } from "@moq/signals";
import type MoqWatch from "../../element";

function formatBitrate(bps: number): string {
	if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`;
	if (bps >= 1_000) return `${(bps / 1_000).toFixed(0)} kbps`;
	return `${bps} bps`;
}

export function qualitySelector(parent: Effect, watch: MoqWatch): HTMLElement {
	const wrapper = document.createElement("div");
	wrapper.className = "quality-selector";

	const label = document.createElement("label");
	label.htmlFor = "moq-watch-quality-select";
	label.className = "quality-label";
	label.textContent = "Quality: ";

	const select = document.createElement("select");
	select.id = "moq-watch-quality-select";
	select.className = "quality-select";

	wrapper.append(label, select);

	parent.run((effect) => {
		const catalog = effect.get(watch.backend.video.source.catalog);
		const active = effect.get(watch.backend.video.source.track);
		const renditions = catalog?.renditions ?? {};

		select.replaceChildren();
		const auto = document.createElement("option");
		auto.value = "";
		auto.textContent = "Auto";
		select.appendChild(auto);

		for (const [name, config] of Object.entries(renditions)) {
			const opt = document.createElement("option");
			opt.value = name;
			const dims = config.codedWidth && config.codedHeight ? ` (${config.codedWidth}x${config.codedHeight})` : "";
			const rate = config.bitrate ? ` ${formatBitrate(config.bitrate)}` : "";
			opt.textContent = `${name}${dims}${rate}`;
			select.appendChild(opt);
		}

		select.value = active ?? "";
	});

	parent.event(select, "change", () => {
		const value = select.value || undefined;
		watch.backend.video.source.target.update((prev) => ({ ...prev, name: value }));
	});

	return wrapper;
}
