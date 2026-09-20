import type * as Audio from "../audio";
import type * as Video from "../video";

/** Audio and video captured by a publish source. */
export type Media = {
	video?: Video.Source;
	audio?: Audio.Source;
};
