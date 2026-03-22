import { PRIORITY, type Track } from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";

export type PreviewInfo = {
	name?: string;
	avatar?: string;
	audio?: boolean;
	video?: boolean;
	screen?: boolean;
	typing?: boolean;
	chat?: boolean;
};

export type PreviewProps = {
	enabled?: boolean | Signal<boolean>;
	info?: PreviewInfo | Signal<PreviewInfo | undefined>;
};

export class Preview {
	static readonly TRACK = "preview.json";
	static readonly PRIORITY = PRIORITY.preview;

	enabled: Signal<boolean>;
	info: Signal<PreviewInfo | undefined>;

	catalog = new Signal<Track | undefined>(undefined);

	signals = new Effect();

	constructor(props?: PreviewProps) {
		this.enabled = Signal.from(props?.enabled ?? false);
		this.info = Signal.from(props?.info);

		this.signals.run((effect) => {
			if (!effect.get(this.enabled)) return;
			effect.set(this.catalog, { name: Preview.TRACK });
		});
	}

	serve(track: Moq.Track, effect: Effect): void {
		const values = effect.getAll([this.enabled, this.info]);
		if (!values) return;
		const [_, info] = values;

		track.writeJson(info);
	}

	close() {
		this.signals.close();
	}
}
