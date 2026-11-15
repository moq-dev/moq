import { Duration } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import { Legacy } from "@moq/hang/container";
import { Origin, Path, Time } from "@moq/net";
import { Signal } from "@moq/signals";
import { Broadcast } from "../../../js/watch/src/broadcast";
import { Sync } from "../../../js/watch/src/sync";
import { Renderer } from "../../../js/watch/src/text/renderer";
import { Source } from "../../../js/watch/src/text/source";

/** Caption selection controls exercised by the browser harness. */
export function captions() {
	const origin = new Origin.Producer();
	const producerRoute = origin.route(Path.from("captions"), { hidden: true });
	const producer = producerRoute.broadcast;
	const textCatalog: NonNullable<Catalog.Root["text"]> = { renditions: {} };
	const catalog: Catalog.Root = { text: textCatalog };
	for (const [name, text] of [
		["first", "First caption"],
		["second", "Second caption"],
	]) {
		textCatalog.renditions[name] = { format: "utf8", role: "subtitle", container: { kind: "legacy" } };
		const track = producer.createTrack(name);
		const group = track.appendGroup();
		group.writeFrame({
			payload: Legacy.encodeFrame(new TextEncoder().encode(text), Duration.micros(1_000_000)),
			timestamp: Time.Timestamp.unscaled(group.sequence),
		});
		group.finish();
	}
	producerRoute.announce();
	const broadcast = new Broadcast({ origin, name: Path.from("captions"), catalogFormat: "manual", catalog });
	const target = new Signal<string | undefined>(undefined);
	const source = new Source({ broadcast, target });
	const sync = new Sync({ delay: Duration.ZERO });
	// Stands in for the media that anchors the clock the cues show against.
	const media = sync.track({ target: new Signal<Duration | undefined>(undefined) });
	media.received(Duration.millis(1000));
	const container = document.createElement("div");
	container.id = "captions";
	container.style.cssText = "position:relative;width:640px;height:360px";
	document.body.append(container);
	const renderer = new Renderer({ source, sync, container });
	return {
		select(name?: string) {
			target.set(name);
		},
		close() {
			renderer.close();
			source.close();
			broadcast.close();
			media.close();
			sync.close();
			producer.close();
			origin.close();
		},
	};
}

declare global {
	interface Window {
		captions: ReturnType<typeof captions>;
		startCaptions(): void;
	}
}
window.startCaptions = () => {
	window.captions = captions();
};
