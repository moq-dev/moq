import "./highlight";
import "@moq/publish/ui";

// We need to import Web Components with fully-qualified paths because of tree-shaking.
import MoqPublish from "@moq/publish/element";
import MoqPublishSupport from "@moq/publish/support/element";

export { MoqPublish, MoqPublishSupport };

const publish = document.querySelector("moq-publish") as MoqPublish;
const watch = document.getElementById("watch") as HTMLAnchorElement;
const watchName = document.getElementById("watch-name") as HTMLSpanElement;

const urlParams = new URLSearchParams(window.location.search);
const path = urlParams.get("broadcast") ?? urlParams.get("path");
if (path) {
	publish.setAttribute("path", path);
	watch.href = `index.html?broadcast=${path}`;
	watchName.textContent = path;
}
