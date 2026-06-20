import "./highlight";
import "@moq/publish/ui";

import { Json } from "@moq/publish";
// We need to import Web Components with fully-qualified paths because of tree-shaking.
import MoqPublish from "@moq/publish/element";
import MoqPublishSupport from "@moq/publish/support/element";

export { MoqPublish, MoqPublishSupport };

const publish = document.querySelector("moq-publish") as MoqPublish;

// ---------------------------------------------------------------------------
// Custom meta.json track
// ---------------------------------------------------------------------------
//
// A track-less Json.Producer retains the current value and fans it out to each
// subscriber, seeding late joiners. publishTrack registers it on the broadcast;
// the component's publish loop serves it whenever a viewer requests `meta.json`.
// We advertise the track in the catalog's `metadata` section (the hang catalog
// is a loose schema, so the extra key passes through and base consumers ignore
// it) so the watch inspector knows to subscribe.

const META_TRACK = "meta.json";

const meta = new Json.Producer<unknown>({
	initial: { title: "My Broadcast", location: "earth", note: "edit me" },
});

publish.broadcast.publishTrack(META_TRACK, (track, effect) => meta.serve(track, effect));
publish.broadcast.catalog.mutate((catalog) => {
	(catalog as typeof catalog & { metadata?: string[] }).metadata = [META_TRACK];
});

// ---------------------------------------------------------------------------
// Metadata editor: publish the textarea's JSON on the meta.json track
// ---------------------------------------------------------------------------

const metaTextEl = document.getElementById("metadata") as HTMLTextAreaElement;
const metaBtn = document.getElementById("send-meta") as HTMLButtonElement;

metaTextEl.addEventListener("input", () => {
	metaBtn.disabled = false;
});

metaBtn.addEventListener("click", () => {
	try {
		// update() emits a full snapshot first (seeding late joiners), then only
		// merge-patch deltas; a no-op if the value is unchanged.
		meta.update(JSON.parse(metaTextEl.value));
		metaTextEl.setCustomValidity("");
		metaBtn.disabled = true;
	} catch (err) {
		// Keep the button armed so the user can fix and retry.
		metaTextEl.setCustomValidity(`invalid JSON: ${(err as Error).message}`);
		metaTextEl.reportValidity();
	}
});
