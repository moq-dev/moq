import "./highlight";
import { setBasePath } from "@moq/hang-ui/utilities";

setBasePath(__HANG_UI_ASSETS_PATH__);

import "@moq/hang-ui/publish/element";

// We need to import Web Components with fully-qualified paths because of tree-shaking.
import HangPublish from "@moq/hang/publish/element";
import HangSupport from "@moq/hang/support/element";

export { HangPublish, HangSupport };

const publish = document.querySelector("hang-publish") as HangPublish;
const watch = document.getElementById("watch") as HTMLAnchorElement;
const watchName = document.getElementById("watch-name") as HTMLSpanElement;

const urlParams = new URLSearchParams(window.location.search);
const path = urlParams.get("path");
if (path) {
	publish.setAttribute("path", path);
	watch.href = `index.html?path=${path}`;
	watchName.textContent = path;
}
