import { z } from "zod/mini";

/** Accept a string wire enum without letting an added producer value reject the whole object. */
function openEnum<const Value extends string>(values: readonly Value[]) {
	const known = new Set<string>(values);
	return z.pipe(
		z.string(),
		z.transform((value: string): Value | "unknown" => (known.has(value) ? (value as Value) : "unknown")),
	);
}

/** Transmission gear, mirrored from the truck's `drive::Gear`. */
export const GearSchema = z.enum(["neutral", "forward", "reverse"]);
export type Gear = z.infer<typeof GearSchema>;

/** The drive state the truck is currently honoring. */
export const DriveStateSchema = z.object({
	steering: z.number(),
	throttle: z.number(),
	brake: z.number(),
	gear: GearSchema,
});
export type DriveState = z.infer<typeof DriveStateSchema>;

/**
 * Where a truck is, in CARLA world metres: x east, y south, yaw clockwise from
 * +x, speed in m/s. Deliberately not latitude and longitude - see
 * `demo/carla/sim/moq_carla_sim/roadmap.py`.
 */
export const PoseSchema = z.object({
	x: z.number(),
	y: z.number(),
	yaw: z.number(),
	speed: z.number(),
});
export type Pose = z.infer<typeof PoseSchema>;

/**
 * How the truck actually responded, in its own frame: acceleration in m/s²
 * (longitudinal positive forward, lateral positive right) and yaw rate in
 * deg/s.
 */
export const MotionSchema = z.object({
	yaw_rate: z.number(),
	accel_lon: z.number(),
	accel_lat: z.number(),
});
export type Motion = z.infer<typeof MotionSchema>;

/**
 * The truck's most recent contact.
 *
 * `at` is unix milliseconds at the instant contact began. An absolute instant
 * rather than an age, so a parked truck's snapshot stays unchanged - and so a
 * viewer who opens the page long after the impact ages it correctly instead of
 * treating the retained snapshot as a fresh crash.
 */
export const CollisionSchema = z.object({
	at: z.number(),
	other: z.string(),
	impulse: z.number(),
});
export type Collision = z.infer<typeof CollisionSchema>;

/** A camera receive/decode/render latency component echoed by the truck. */
export const LatencyEntrySchema = z.object({
	label: z.string(),
	ms: z.number(),
});

/** Per-update telemetry the truck publishes on the `status` track. */
export const TruckStatusSchema = z.object({
	accepted: DriveStateSchema,
	// Open, so a producer that adds a value degrades to `unknown` instead of
	// rejecting every status from it.
	autonomy: z._default(openEnum(["autonomous", "manual"]), "unknown"),
	estopped: z.boolean(),
	estop_by: z.optional(z.string()),
	driver: z.optional(z.string()),
	sequence: z.optional(z.number()),
	media_pipeline: z.record(z.string(), z.array(LatencyEntrySchema)),
	location: z.optional(z.string()),
	// Absent until the simulator's first physics tick reports where the vehicle is.
	map: z.optional(z.string()),
	pose: z.optional(PoseSchema),
	// `accepted` is the command the gate let through; these are what the
	// vehicle did with it, and arrive whoever is driving.
	measured: z.optional(DriveStateSchema),
	motion: z.optional(MotionSchema),
	collision: z.optional(CollisionSchema),
});
export type TruckStatus = z.infer<typeof TruckStatusSchema>;
