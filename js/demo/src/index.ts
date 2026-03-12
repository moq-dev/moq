import "./highlight";
import "@moq/watch/ui";
import MoqWatch from "@moq/watch/element";
import MoqWatchSupport from "@moq/watch/support/element";
import MoqAnnounced from "./announced";

export { MoqWatchSupport, MoqWatch, MoqAnnounced };

const watch = document.querySelector("moq-watch") as MoqWatch | undefined;
const announced = document.querySelector("moq-announced") as MoqAnnounced | undefined;

if (!watch) throw new Error("unable to find <moq-watch> element");

// If query params are provided, use them.
const urlParams = new URLSearchParams(window.location.search);
const name = urlParams.get("broadcast") ?? urlParams.get("name");
const url = urlParams.get("url");

if (url) watch.setAttribute("url", url);
if (name) watch.setAttribute("name", name);

// Wire the announced component to use the watch element's connection.
if (announced) announced.watch = watch;
