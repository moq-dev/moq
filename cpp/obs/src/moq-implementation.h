// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <moq/moq.hpp>

#include <string>

// CMake defines it from the plugin's VERSION; a build outside CMake reports dev.
#ifndef PLUGIN_VERSION_STRING
#define PLUGIN_VERSION_STRING "dev"
#endif

// What the plugin's sessions advertise in SETUP ahead of moq-ffi's: itself, then the C++ package.
inline std::string MoQImplementation()
{
	return std::string("moq-obs/" PLUGIN_VERSION_STRING " ") + moq::IMPLEMENTATION;
}
