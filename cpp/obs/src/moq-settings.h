// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include <obs-module.h>

#include <string>
#include <vector>

extern "C" {
#include "moq.h"
}

// Advanced MoQ connection settings.
//
// One set of keys on an obs_data_t backs every surface that edits them: the service
// properties page, the dock's advanced dialog, and the output that reads them at connect
// time. The fields are described once in Fields() so both UIs are generated from the
// same list; adding a knob means adding a Field entry and one assignment in BuildConfig.
namespace MoQSettings {

// Whether any of this applies. With it off the output dials with libmoq's defaults and
// ignores every other key.
inline constexpr const char *ENABLED = "advanced";

// The value a Choice field carries when the user hasn't picked: leave the knob wherever
// the library puts it. An empty string, so it round-trips through obs_data as "no
// choice" without a separate flag.
inline constexpr const char *AUTO = "";

// What kind of widget a field needs.
enum class Kind {
	Bool,
	Int,
	Text,
	// A file path; Filter names what to accept.
	File,
	Directory,
	// A fixed set of values, or a suggested set when Editable is set.
	Choice,
};

// One entry in a Choice field's menu.
struct Option {
	const char *label;
	const char *value;
};

// A single advanced setting, rendered by both UIs and read by BuildConfig.
struct Field {
	const char *key;
	const char *label;
	// Longer explanation, shown as a tooltip. May be null.
	const char *tooltip;
	Kind kind;

	// Int only.
	long long min;
	long long max;
	long long step;

	// Defaults. Only the one matching kind is meaningful.
	bool bool_default;
	long long int_default;
	const char *text_default;

	// Choice only. Editable lets the user type a value that isn't listed.
	std::vector<Option> options;
	bool editable;

	// File only: an OBS path filter, e.g. "PEM (*.pem);;All (*)".
	const char *filter;
};

// Every advanced setting, in display order.
//
// Built on first call because the protocol version menu is populated from libmoq, so it
// can't drift from what the library actually accepts.
const std::vector<Field> &Fields();

// Register the defaults from Fields(). Call from the service's get_defaults.
void Defaults(obs_data_t *settings);

// Add the checkable "Advanced" group to a properties list.
void AddProperties(obs_properties_t *props);

// A moq_client_config plus the storage its pointers borrow.
//
// libmoq reads the strings while dialing rather than copying them up front, so this
// has to outlive the moq_session_connect call it is passed to. Keep it on the stack
// across the dial and let it go afterwards.
struct Config {
	// What to pass to moq_session_connect: NULL when the advanced group is off,
	// which dials with the library defaults.
	const moq_client_config *Pointer() const { return enabled ? &value : nullptr; }

	moq_client_config value{};
	bool enabled = false;

	// Backing storage for the pointers in `value`. Held by value so a settings
	// object released after BuildConfig can't dangle them.
	std::string version, backend, bind, fingerprint, root, host_name, congestion, qlog;
	moq_string version_item{}, fingerprint_item{}, root_item{};
};

// Fill `out` from these settings.
//
// Returns false if libmoq never reported the defaults the fields are built from; the
// caller should refuse to start rather than dial with a setting the user asked for
// silently dropped. Values themselves are validated by the dial, not here, so a bad
// fingerprint or bind address surfaces as a moq_session_connect failure with the
// reason in moq_error().
bool BuildConfig(obs_data_t *settings, Config *out);

} // namespace MoQSettings
