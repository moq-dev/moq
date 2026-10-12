// Load caption rendering and its styles only for a selected text track.
export { CaptionsRenderer, parseText, VTTCue } from "media-captions";

import captionsCss from "media-captions/styles/captions.css?inline";
import regionsCss from "media-captions/styles/regions.css?inline";
export const OVERLAY_CSS = `${captionsCss}\n${regionsCss}`;
