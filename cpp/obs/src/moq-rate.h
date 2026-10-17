// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <optional>

// Turns a reservation's grant into the bitrate an OBS encoder should run at,
// with the same shape as moq_mux::rate::Control: drops apply at once, raises
// climb at most 25% per second, and moves under 5% are ignored so a jittering
// grant doesn't retune the encoder on every frame. Keep the two in step.
class MoQRate {
public:
	using Clock = std::chrono::steady_clock;

	// Starts at `max`, the configured bitrate, which the grant can only lower.
	// The floor is a tenth of it: below that the video isn't worth sending.
	explicit MoQRate(uint64_t max) : max(max), min(max / 10), target(max) {}

	uint64_t Target() const { return target; }
	uint64_t Max() const { return max; }

	// Feed a grant, returning the new target when it moved enough to apply.
	// A missing grant (no estimate, or no subscriber) holds the target: losing
	// the estimate is not evidence the uplink got better.
	std::optional<uint64_t> Update(std::optional<uint64_t> grant, Clock::time_point now)
	{
		if (!grant)
			return std::nullopt;

		const uint64_t desired = std::clamp(*grant, min, max);
		uint64_t next = desired;
		if (desired > target && applied) {
			const double elapsed = std::chrono::duration<double>(now - *applied).count();
			const auto grown = static_cast<uint64_t>(static_cast<double>(target) *
								 (1.0 + RAMP * std::max(elapsed, 0.0)));
			next = std::clamp(std::min(grown, desired), min, max);
		}

		// Hysteresis leaves `applied` alone, so suppressed raises keep accruing
		// ramp allowance. The final step onto `max` is exempt, or a recovery
		// would stall a few percent under the configured rate for good.
		const bool settling = next > target && next == max;
		const uint64_t diff = next > target ? next - target : target - next;
		if (!settling && static_cast<double>(diff) < static_cast<double>(target) * HYSTERESIS)
			return std::nullopt;

		target = next;
		applied = now;
		return next;
	}

private:
	static constexpr double HYSTERESIS = 0.05;
	static constexpr double RAMP = 0.25;

	uint64_t max;
	uint64_t min;
	uint64_t target;
	std::optional<Clock::time_point> applied;
};
