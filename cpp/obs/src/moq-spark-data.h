// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <deque>
#include <optional>

struct MoQSparkData {
	struct Sample {
		bool valid;
		double value;
	};
	std::deque<Sample> samples;

	void push(bool valid, double value)
	{
		samples.push_back({valid, value});
		while (samples.size() > 60)
			samples.pop_front();
	}

	std::optional<double> latest() const
	{
		if (samples.empty() || !samples.back().valid)
			return std::nullopt;
		return samples.back().value;
	}
};
