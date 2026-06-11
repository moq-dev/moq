import "./highlight";
import "@moq/publish/ui";

import { Source } from "@moq/publish";

// We need to import Web Components with fully-qualified paths because of tree-shaking.
import MoqPublish from "@moq/publish/element";
import MoqPublishSupport from "@moq/publish/support/element";

export { MoqPublish, MoqPublishSupport };

const publish = document.querySelector("moq-publish") as MoqPublish;
const watch = document.getElementById("watch") as HTMLAnchorElement;
const watchName = document.getElementById("watch-name") as HTMLSpanElement;

// Ask the camera for 720p. Most browsers default to 640x480, which is already below
// the sd rendition's 480p cap, so hd and sd would otherwise be nearly identical.
publish.video.watch((video) => {
	if (video instanceof Source.Camera) {
		video.constraints.set({ width: { ideal: 1280 }, height: { ideal: 720 } });
	}
});

const urlParams = new URLSearchParams(window.location.search);
const name = urlParams.get("broadcast") ?? urlParams.get("name");
if (name) {
	publish.setAttribute("name", name);
	watch.href = `index.html?broadcast=${name}`;
	watchName.textContent = name;
}
