// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once
#include <obs-module.h>

#include <vector>

// Advanced MoQ connection settings.
//
// One set of keys on an obs_data_t backs every surface that edits them: the service
// properties page, the dock's advanced dialog, and the output that reads them at connect
// time. The fields are described once in Fields() so both UIs are generated from the
// same list; adding a knob means adding a Field entry and one call in CreateClient.
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

// A single advanced setting, rendered by both UIs and read by CreateClient.
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

// Build a libmoq client config handle from these settings.
//
// Returns 0 when the advanced group is off, meaning "dial with the defaults" (which is
// what a 0 client handle means to moq_client_connect). Returns negative if a setting is
// invalid, or if libmoq never reported the defaults the fields are built from; the
// caller should refuse to start rather than connect with a setting the user asked for
// silently dropped. The caller owns the handle and must release it with
// moq_client_close.
int CreateClient(obs_data_t *settings);

} // namespace MoQSettings
