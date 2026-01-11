import type { JSX } from "solid-js";

import arrowDown from "./arrow-down.svg?raw";
import arrowUp from "./arrow-up.svg?raw";
import audio from "./audio.svg?raw";
import ban from "./ban.svg?raw";
import buffer from "./buffer.svg?raw";
import camera from "./camera.svg?raw";
import file from "./file.svg?raw";
import fullscreenEnter from "./fullscreen-enter.svg?raw";
import fullscreenExit from "./fullscreen-exit.svg?raw";
import microphone from "./microphone.svg?raw";
import mute from "./mute.svg?raw";
import network from "./network.svg?raw";
import pause from "./pause.svg?raw";
import play from "./play.svg?raw";
import screen from "./screen.svg?raw";
import stats from "./stats.svg?raw";
import video from "./video.svg?raw";
import volumeHigh from "./volume-high.svg?raw";
import volumeLow from "./volume-low.svg?raw";
import volumeMedium from "./volume-medium.svg?raw";

/**
 * Given the SVG source of an icon, return a JSX.Element
 *
 * @returns JSX.Element
 */
export function Element(src: string): JSX.Element {
	return <span classList={{ "flex--center": true }} role="img" aria-hidden={true} innerHTML={src} />;
}

// For each icon, export a function that returns a JSX.Element
const icon = (src: string) => () => Element(src);

export const ArrowDown = icon(arrowDown);
export const ArrowUp = icon(arrowUp);
export const Audio = icon(audio);
export const Ban = icon(ban);
export const Buffer = icon(buffer);
export const Camera = icon(camera);
export const File = icon(file);
export const FullscreenEnter = icon(fullscreenEnter);
export const FullscreenExit = icon(fullscreenExit);
export const Microphone = icon(microphone);
export const Mute = icon(mute);
export const Network = icon(network);
export const Pause = icon(pause);
export const Play = icon(play);
export const Screen = icon(screen);
export const Stats = icon(stats);
export const Video = icon(video);
export const VolumeHigh = icon(volumeHigh);
export const VolumeLow = icon(volumeLow);
export const VolumeMedium = icon(volumeMedium);
