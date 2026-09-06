// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-spark-data.h"

#include <cassert>
#include <cstdio>

int main()
{
	MoQSparkData data;
	assert(!data.latest());
	data.push(true, 12.5);
	assert(data.latest() == 12.5);
	data.push(false, 0);
	assert(!data.latest());
	assert(data.samples.front().valid && !data.samples.back().valid);
	data.push(true, 20);
	assert(data.latest() == 20);
	for (int i = 0; i < 60; i++)
		data.push(false, 0);
	assert(data.samples.size() == 60 && !data.latest());
	data.samples.clear();
	assert(!data.latest());
	std::puts("offline spark values clear while history retains gaps: ok");
}
