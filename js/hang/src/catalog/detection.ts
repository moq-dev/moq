import * as z from "zod/mini";
import { TrackSchema } from "./track";

// Detection metadata in the catalog.
//
// This describes a track containing live object-detection results
// (bounding boxes), typically produced by an AI worker analyzing the
// video stream. The catalog only carries the track reference; the
// actual detections are sent as JSON frames on the referenced track,
// matching the DetectionsSchema below.
export const DetectionSchema = z.object({
	track: z.optional(TrackSchema),
});

export type Detection = z.infer<typeof DetectionSchema>;

// A single detected object in a video frame.
// Coordinates are normalized between 0 and 1 relative to the video
// frame's coded dimensions, with (0,0) at the top-left corner.
export const DetectionBoxSchema = z.object({
	x: z.number(),
	y: z.number(),
	w: z.number(),
	h: z.number(),
	label: z.optional(z.string()),
	score: z.optional(z.number()),
});

export type DetectionBox = z.infer<typeof DetectionBoxSchema>;

// A frame on the detection track: the bounding boxes detected in a
// single video frame, optionally tagged with the source video PTS in
// microseconds.
export const DetectionsSchema = z.object({
	timestamp: z.optional(z.number()),
	boxes: z.array(DetectionBoxSchema),
});

export type Detections = z.infer<typeof DetectionsSchema>;
