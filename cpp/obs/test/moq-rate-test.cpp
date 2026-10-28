// SPDX-License-Identifier: GPL-2.0-or-later
//
// The cases moq_mux::rate::Control is tested with, so the two policies agree.
#include "moq-rate.h"

#include <cstdio>
#include <cstdlib>

using namespace std::chrono_literals;

namespace {
int g_failures = 0;

#define CHECK(cond)                                                                 \
	do {                                                                        \
		if (!(cond)) {                                                      \
			fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
			g_failures++;                                               \
		}                                                                   \
	} while (0)

const MoQRate::Clock::time_point START{};
} // namespace

int main()
{
	{
		// Starts at the configured rate and drops at once.
		MoQRate rate(4'000'000);
		CHECK(rate.Target() == 4'000'000);
		CHECK(rate.Update(2'000'000, START) == 2'000'000);
	}
	{
		// A missing grant holds, however long it lasts.
		MoQRate rate(4'000'000);
		rate.Update(2'000'000, START);
		CHECK(!rate.Update(std::nullopt, START + 10s));
		CHECK(rate.Target() == 2'000'000);
	}
	{
		// Never above the configured rate, never below a tenth of it.
		MoQRate rate(4'000'000);
		CHECK(!rate.Update(100'000'000, START));
		CHECK(rate.Update(1, START) == 400'000);
	}
	{
		// One second of ramp allows 1M -> 1.25M, not the whole grant.
		MoQRate rate(4'000'000);
		rate.Update(1'000'000, START);
		CHECK(rate.Update(4'000'000, START + 1s) == 1'250'000);
	}
	{
		// Each 100ms tick alone is under the 5% deadband; suppression must keep
		// the ramp anchor so the raise lands on the second tick.
		MoQRate rate(4'000'000);
		rate.Update(1'000'000, START);
		CHECK(!rate.Update(4'000'000, START + 100ms));
		CHECK(rate.Update(4'000'000, START + 200ms) == 1'050'000);
	}
	{
		// A recovery lands exactly on the configured rate, then stops retuning.
		MoQRate rate(4'000'000);
		rate.Update(1'000'000, START);
		std::optional<uint64_t> last;
		for (int tick = 1; tick <= 200; tick++) {
			if (auto next = rate.Update(4'000'000, START + tick * 100ms))
				last = next;
		}
		CHECK(last == 4'000'000);
		CHECK(!rate.Update(4'000'000, START + 60s));
	}
	{
		// Small moves either way stay inside the deadband.
		MoQRate rate(4'000'000);
		rate.Update(2'000'000, START);
		CHECK(!rate.Update(2'010'000, START + 100ms));
		CHECK(!rate.Update(1'960'000, START + 1s));
		CHECK(rate.Update(1'600'000, START + 2s) == 1'600'000);
	}

	if (g_failures) {
		fprintf(stderr, "%d rate check(s) failed\n", g_failures);
		return EXIT_FAILURE;
	}
	std::puts("rate follows the grant like moq_mux::rate::Control: ok");
	return EXIT_SUCCESS;
}
