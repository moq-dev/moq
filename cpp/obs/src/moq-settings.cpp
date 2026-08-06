// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-settings.h"
#include "logger.h"

#include <algorithm>
#include <cstring>
#include <string>
#include <vector>

extern "C" {
#include "moq.h"
}

namespace MoQSettings {

namespace {

// Keys. These are stable: they land in scene collections and in the dock's settings
// file, so renaming one silently drops whatever the user had configured.
constexpr const char *VERSION = "version";
constexpr const char *BACKEND = "backend";
constexpr const char *BIND = "bind";
constexpr const char *CONNECT_TIMEOUT = "connect_timeout_ms";
constexpr const char *FAILOVER_DELAY = "failover_delay_ms";

constexpr const char *TLS_DISABLE_VERIFY = "tls_disable_verify";
constexpr const char *TLS_FINGERPRINT = "tls_fingerprint";
constexpr const char *TLS_ROOT = "tls_root";
constexpr const char *TLS_HOST_NAME = "tls_host_name";

constexpr const char *BACKOFF_INITIAL = "backoff_initial_ms";
constexpr const char *BACKOFF_MAX = "backoff_max_ms";
constexpr const char *BACKOFF_TIMEOUT = "backoff_timeout_ms";

constexpr const char *QUIC_CONGESTION_CONTROL = "quic_congestion_control";
constexpr const char *QUIC_MAX_STREAMS = "quic_max_streams";
constexpr const char *QUIC_IDLE_TIMEOUT = "quic_idle_timeout_ms";
constexpr const char *QUIC_KEEP_ALIVE = "quic_keep_alive_ms";
constexpr const char *QUIC_GSO = "quic_gso";
constexpr const char *QUIC_MTU_DISCOVERY = "quic_mtu_discovery";
constexpr const char *QUIC_QLOG = "quic_qlog";

constexpr const char *WEBSOCKET_ENABLED = "websocket_enabled";
constexpr const char *WEBSOCKET_DELAY = "websocket_delay_ms";

// Read a string setting, returning nullptr when it's unset or empty. libmoq's optional
// string setters take exactly that: NULL means "leave at the default".
const char *OptionalString(obs_data_t *settings, const char *key)
{
	const char *value = obs_data_get_string(settings, key);
	return (value && *value) ? value : nullptr;
}

// Resolve a tri-state choice (AUTO / "on" / "off") into an override, if any.
bool TriState(obs_data_t *settings, const char *key, bool *out)
{
	const char *value = OptionalString(settings, key);
	if (!value)
		return false;
	*out = strcmp(value, "on") == 0;
	return true;
}

// Read an integer setting inside the range declared by its Field. Scene collections
// are editable JSON, so the widget bounds alone do not protect the unsigned C API.
uint64_t Amount(obs_data_t *settings, const char *key)
{
	const long long value = obs_data_get_int(settings, key);
	for (const Field &field : Fields()) {
		if (strcmp(field.key, key) == 0)
			return static_cast<uint64_t>(std::clamp(value, field.min, field.max));
	}

	LOG_ERROR("Advanced integer setting has no Field: %s", key);
	return 0;
}

// Report a failed libmoq call with the reason it recorded. moq_error() only describes
// the most recent call on this thread, so read it before anything else runs.
void LogFailure(const char *what, int code)
{
	const char *reason = moq_error();
	LOG_ERROR("Advanced setting %s rejected (%d): %s", what, code, reason ? reason : "(no reason)");
}

// Enumerate a libmoq capability into menu options. The names live beside the caller's
// static field table because Option borrows their pointers.
std::vector<Option> CapabilityOptions(int (*enumerate)(moq_string *, size_t), const char *automatic,
				      std::vector<std::string> &names)
{
	std::vector<Option> options = {{automatic, AUTO}};

	int count = enumerate(nullptr, 0);
	if (count <= 0)
		return options;

	std::vector<moq_string> raw(static_cast<size_t>(count));
	const int reported = enumerate(raw.data(), raw.size());
	if (reported < 0)
		return options;
	const size_t written = std::min(raw.size(), static_cast<size_t>(reported));

	names.clear();
	names.reserve(written);
	for (size_t i = 0; i < written; ++i)
		names.emplace_back(raw[i].data, raw[i].len);

	for (const std::string &name : names)
		options.push_back({name.c_str(), name.c_str()});
	return options;
}

// The protocol versions this build of libmoq offers, so the menu can't drift from what
// moq_client_set_versions accepts.
std::vector<Option> VersionOptions()
{
	static std::vector<std::string> names;
	return CapabilityOptions(moq_versions, "Automatic (offer all)", names);
}

// The QUIC backends this build of libmoq compiled in, so the menu can't offer one the
// setter rejects. The backends are feature-gated, so listing the three names would put
// dead options in front of the user on a release build.
std::vector<Option> BackendOptions()
{
	static std::vector<std::string> names;
	return CapabilityOptions(moq_backends, "Automatic", names);
}

// Shorthands so the table below reads as data rather than aggregate initializers.
Field Toggle(const char *key, const char *label, bool value, const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::Bool, 0, 0, 0, value, 0, "", {}, false, nullptr};
}

Field Number(const char *key, const char *label, long long value, long long min, long long max, long long step,
	     const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::Int, min, max, step, false, value, "", {}, false, nullptr};
}

Field Text(const char *key, const char *label, const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::Text, 0, 0, 0, false, 0, "", {}, false, nullptr};
}

Field Choose(const char *key, const char *label, std::vector<Option> options, bool editable = false,
	     const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::Choice, 0, 0, 0, false, 0, AUTO, std::move(options), editable, nullptr};
}

Field File(const char *key, const char *label, const char *filter, const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::File, 0, 0, 0, false, 0, "", {}, false, filter};
}

Field Directory(const char *key, const char *label, const char *tooltip = nullptr)
{
	return Field{key, label, tooltip, Kind::Directory, 0, 0, 0, false, 0, "", {}, false, nullptr};
}

// The tri-state used wherever the library default depends on the backend.
std::vector<Option> AutoOnOff()
{
	return {{"Automatic", AUTO}, {"Enabled", "on"}, {"Disabled", "off"}};
}

// libmoq's own defaults, so the table below doesn't repeat numbers that live in
// moq-native and drift the moment one is retuned. Same reason Fields() reads the
// version list from moq_versions instead of listing drafts.
//
// A knob nobody has set reads back as its default, so a throwaway handle is the
// defaults. Local to the plugin rather than a libmoq type: the ABI is the getters.
struct DefaultValues {
	long long connect_timeout_ms = 0;
	long long failover_delay_ms = 0;
	long long backoff_initial_ms = 0;
	long long backoff_max_ms = 0;
	long long backoff_timeout_ms = 0;
	long long quic_max_streams = 0;
	long long quic_idle_timeout_ms = 0;
	long long quic_keep_alive_ms = 0;
	long long websocket_delay_ms = 0;
	bool websocket_enabled = false;
	bool loaded = false;
};

// Read a uint64 knob into the signed integer the UI edits, tracking whether every
// getter in the batch succeeded.
void Read(int (*get)(uint32_t, uint64_t *), uint32_t client, long long *out, bool *ok, const char *what)
{
	uint64_t value = 0;
	int ret = get(client, &value);
	if (ret < 0) {
		const char *reason = moq_error();
		LOG_ERROR("Failed to read the %s default (%d): %s", what, ret, reason ? reason : "(no reason)");
		*ok = false;
		return;
	}
	*out = (long long)value;
}

// The library's defaults, read once. A failure leaves `loaded` false, which
// CreateClient refuses on rather than writing zeros into a scene collection.
const DefaultValues &LibraryDefaults()
{
	static const DefaultValues defaults = [] {
		DefaultValues d;
		int client = moq_client_create();
		if (client < 0) {
			LOG_ERROR("Failed to create a libmoq client to read defaults from: %d", client);
			return d;
		}

		bool ok = true;
		uint32_t handle = (uint32_t)client;
		Read(moq_client_get_connect_timeout, handle, &d.connect_timeout_ms, &ok, "connect timeout");
		Read(moq_client_get_failover_delay, handle, &d.failover_delay_ms, &ok, "Happy Eyeballs delay");
		Read(moq_client_get_backoff_initial, handle, &d.backoff_initial_ms, &ok, "reconnect delay");
		Read(moq_client_get_backoff_max, handle, &d.backoff_max_ms, &ok, "reconnect delay cap");
		Read(moq_client_get_backoff_timeout, handle, &d.backoff_timeout_ms, &ok, "reconnect timeout");
		Read(moq_client_get_quic_max_streams, handle, &d.quic_max_streams, &ok, "max concurrent streams");
		Read(moq_client_get_quic_idle_timeout, handle, &d.quic_idle_timeout_ms, &ok, "idle timeout");
		Read(moq_client_get_quic_keep_alive, handle, &d.quic_keep_alive_ms, &ok, "keep-alive interval");
		Read(moq_client_get_websocket_delay, handle, &d.websocket_delay_ms, &ok, "WebSocket fallback delay");

		int ret = moq_client_get_websocket_enabled(handle, &d.websocket_enabled);
		if (ret < 0) {
			LOG_ERROR("Failed to read the WebSocket fallback default: %d", ret);
			ok = false;
		}

		moq_client_close(client);
		d.loaded = ok;
		return d;
	}();

	return defaults;
}

} // namespace

const std::vector<Field> &Fields()
{
	static const std::vector<Field> fields = [] {
		const DefaultValues &d = LibraryDefaults();
		std::vector<Field> f;

		// Protocol. Editable so a work-in-progress draft, which is never offered by
		// default and so never listed, can still be typed in.
		f.push_back(Choose(VERSION, "Protocol version", VersionOptions(), true,
				   "Pin the handshake to one draft instead of offering every supported version."));
		f.push_back(Choose(BACKEND, "QUIC backend", BackendOptions()));
		f.push_back(Text(BIND, "Bind address",
				 "Local UDP address to send from, e.g. 192.0.2.7:0 to pin the outgoing "
				 "interface. Leave empty for any."));
		f.push_back(Number(CONNECT_TIMEOUT, "Connect timeout (ms)", d.connect_timeout_ms, 0, 600000, 1000,
				   "How long one connection attempt may take, dial and handshake together. "
				   "0 waits forever."));
		f.push_back(Number(FAILOVER_DELAY, "Happy Eyeballs delay (ms)", d.failover_delay_ms, 0, 10000, 50,
				   "How long to wait before also trying the next address DNS returned."));

		// TLS.
		f.push_back(Toggle(TLS_DISABLE_VERIFY, "Skip certificate verification", false,
				   "Development only: accepts any certificate, so it cannot be combined "
				   "with a fingerprint or a root below. To trust one known self-signed "
				   "relay, turn this off and pin its fingerprint instead."));
		f.push_back(Text(TLS_FINGERPRINT, "Certificate fingerprint (SHA-256 hex)",
				 "Trust a self-signed certificate by pinning its fingerprint, without "
				 "accepting every certificate the way the option above does. Leave that "
				 "option off, or the stream refuses to start rather than quietly "
				 "ignoring this pin."));
		f.push_back(File(TLS_ROOT, "Root certificate (PEM)", "PEM (*.pem *.crt);;All Files (*)",
				 "Trust this CA instead of the system roots."));
		f.push_back(Text(TLS_HOST_NAME, "Server name override (SNI)",
				 "Validate the certificate against this name instead of the host in the "
				 "URL, so a relay can be reached by IP."));

		// Reconnect.
		f.push_back(Number(BACKOFF_INITIAL, "Reconnect delay (ms)", d.backoff_initial_ms, 1, 60000, 100,
				   "Delay before the first reconnect attempt; it grows from here."));
		f.push_back(Number(BACKOFF_MAX, "Reconnect delay cap (ms)", d.backoff_max_ms, 1, 600000, 1000,
				   "Ceiling on the growing reconnect delay."));
		f.push_back(Number(BACKOFF_TIMEOUT, "Give up after (ms)", d.backoff_timeout_ms, 0, 3600000, 1000,
				   "Total time to keep retrying before the stream fails. Also how long the "
				   "broadcast lingers for viewers across the gap. 0 retries forever."));

		// QUIC transport.
		f.push_back(Choose(
			QUIC_CONGESTION_CONTROL, "Congestion control",
			{{"Automatic", AUTO}, {"Delay-based (BBR)", "delay"}, {"Loss-based (CUBIC)", "loss"}}, false,
			"Delay-based keeps queues short and the send rate steady enough for an "
			"encoder to track. Loss-based chases throughput instead."));
		f.push_back(Number(QUIC_MAX_STREAMS, "Max concurrent streams", d.quic_max_streams, 1, 65536, 1,
				   "MoQ opens a stream per group, so a busy publisher wants this high."));
		f.push_back(Number(QUIC_IDLE_TIMEOUT, "Idle timeout (ms)", d.quic_idle_timeout_ms, 0, 600000, 1000,
				   "How long an idle connection survives before it is dropped."));
		f.push_back(Number(QUIC_KEEP_ALIVE, "Keep-alive interval (ms)", d.quic_keep_alive_ms, 0, 60000, 100,
				   "0 disables the keep-alive pings."));
		f.push_back(Choose(QUIC_GSO, "UDP segmentation offload", AutoOnOff(), false,
				   "Batches sends into one syscall. Turn it off if large sends vanish; some "
				   "NICs and middleboxes mangle segmented packets."));
		f.push_back(Choose(QUIC_MTU_DISCOVERY, "Path MTU discovery", AutoOnOff()));
		// Only when this build can capture: setting a directory without support
		// fails the dial, so offering it would be a control that only breaks things.
		if (moq_qlog_supported()) {
			f.push_back(Directory(QUIC_QLOG, "qlog directory",
					      "Write QUIC connection traces here for diagnosing stalls. Leave "
					      "empty to disable; the files get large."));
		}

		// WebSocket fallback.
		f.push_back(Toggle(WEBSOCKET_ENABLED, "WebSocket fallback", d.websocket_enabled,
				   "Race a WebSocket connection against QUIC so a network that blocks UDP "
				   "still goes live. Turn it off to measure the QUIC path alone."));
		f.push_back(Number(WEBSOCKET_DELAY, "WebSocket fallback delay (ms)", d.websocket_delay_ms, 0, 10000, 50,
				   "How long QUIC gets a head start before the WebSocket attempt joins in."));

		return f;
	}();

	return fields;
}

void Defaults(obs_data_t *settings)
{
	obs_data_set_default_bool(settings, ENABLED, false);

	for (const Field &field : Fields()) {
		switch (field.kind) {
		case Kind::Bool:
			obs_data_set_default_bool(settings, field.key, field.bool_default);
			break;
		case Kind::Int:
			obs_data_set_default_int(settings, field.key, field.int_default);
			break;
		case Kind::Text:
		case Kind::File:
		case Kind::Directory:
		case Kind::Choice:
			obs_data_set_default_string(settings, field.key, field.text_default);
			break;
		}
	}
}

void AddProperties(obs_properties_t *props)
{
	obs_properties_t *group = obs_properties_create();

	for (const Field &field : Fields()) {
		obs_property_t *p = nullptr;

		switch (field.kind) {
		case Kind::Bool:
			p = obs_properties_add_bool(group, field.key, field.label);
			break;
		case Kind::Int:
			p = obs_properties_add_int(group, field.key, field.label, (int)field.min, (int)field.max,
						   (int)field.step);
			break;
		case Kind::Text:
			p = obs_properties_add_text(group, field.key, field.label, OBS_TEXT_DEFAULT);
			break;
		case Kind::File:
			p = obs_properties_add_path(group, field.key, field.label, OBS_PATH_FILE, field.filter,
						    nullptr);
			break;
		case Kind::Directory:
			p = obs_properties_add_path(group, field.key, field.label, OBS_PATH_DIRECTORY, nullptr,
						    nullptr);
			break;
		case Kind::Choice:
			p = obs_properties_add_list(group, field.key, field.label,
						    field.editable ? OBS_COMBO_TYPE_EDITABLE : OBS_COMBO_TYPE_LIST,
						    OBS_COMBO_FORMAT_STRING);
			for (const Option &option : field.options)
				obs_property_list_add_string(p, option.label, option.value);
			break;
		}

		if (p && field.tooltip)
			obs_property_set_long_description(p, field.tooltip);
	}

	obs_properties_add_group(props, ENABLED, "Advanced", OBS_GROUP_CHECKABLE, group);
}

int CreateClient(obs_data_t *settings)
{
	if (!settings || !obs_data_get_bool(settings, ENABLED))
		return 0;

	// Every numeric field defaulted to zero, so the settings on hand describe a config
	// nobody chose. Refusing beats dialing with a 0ms idle timeout the user never asked
	// for. LibraryDefaults() already logged which getter failed. Plain -1 because
	// libmoq's error codes are Rust-side only; moq.h exports no names for them.
	if (!LibraryDefaults().loaded) {
		LOG_ERROR("Advanced settings unavailable: libmoq did not report its defaults");
		return -1;
	}

	int client = moq_client_create();
	if (client < 0) {
		LogFailure("client", client);
		return client;
	}

	// Any failure below aborts: the user opted into these settings explicitly, and a
	// silently dropped version pin or fingerprint is the exact confusion the advanced
	// settings exist to avoid.
	int code = 0;
	auto fail = [&](const char *what) {
		LogFailure(what, code);
		moq_client_close(client);
		return code;
	};

	if (const char *version = OptionalString(settings, VERSION)) {
		moq_string one = {version, strlen(version)};
		code = moq_client_set_versions(client, &one, 1);
		if (code < 0)
			return fail("protocol version");
	}

	if (const char *backend = OptionalString(settings, BACKEND)) {
		code = moq_client_set_backend(client, backend, strlen(backend));
		if (code < 0)
			return fail("QUIC backend");
	}

	if (const char *bind = OptionalString(settings, BIND)) {
		code = moq_client_set_bind(client, bind, strlen(bind));
		if (code < 0)
			return fail("bind address");
	}

	code = moq_client_set_connect_timeout(client, Amount(settings, CONNECT_TIMEOUT));
	if (code < 0)
		return fail("connect timeout");

	code = moq_client_set_failover_delay(client, Amount(settings, FAILOVER_DELAY));
	if (code < 0)
		return fail("Happy Eyeballs delay");

	code = moq_client_set_tls_disable_verify(client, obs_data_get_bool(settings, TLS_DISABLE_VERIFY));
	if (code < 0)
		return fail("certificate verification");

	if (const char *fingerprint = OptionalString(settings, TLS_FINGERPRINT)) {
		moq_string one = {fingerprint, strlen(fingerprint)};
		code = moq_client_set_tls_fingerprints(client, &one, 1);
		if (code < 0)
			return fail("certificate fingerprint");
	}

	if (const char *root = OptionalString(settings, TLS_ROOT)) {
		moq_string one = {root, strlen(root)};
		code = moq_client_set_tls_roots(client, &one, 1);
		if (code < 0)
			return fail("root certificate");
	}

	// NULL clears the override, so this one call covers both set and unset.
	const char *host_name = OptionalString(settings, TLS_HOST_NAME);
	code = moq_client_set_tls_host_name(client, host_name, host_name ? strlen(host_name) : 0);
	if (code < 0)
		return fail("server name override");

	code = moq_client_set_backoff_initial(client, Amount(settings, BACKOFF_INITIAL));
	if (code < 0)
		return fail("reconnect delay");

	code = moq_client_set_backoff_max(client, Amount(settings, BACKOFF_MAX));
	if (code < 0)
		return fail("reconnect delay cap");

	code = moq_client_set_backoff_timeout(client, Amount(settings, BACKOFF_TIMEOUT));
	if (code < 0)
		return fail("reconnect give-up timeout");

	code = moq_client_set_quic_max_streams(client, Amount(settings, QUIC_MAX_STREAMS));
	if (code < 0)
		return fail("max concurrent streams");

	code = moq_client_set_quic_idle_timeout(client, Amount(settings, QUIC_IDLE_TIMEOUT));
	if (code < 0)
		return fail("idle timeout");

	code = moq_client_set_quic_keep_alive(client, Amount(settings, QUIC_KEEP_ALIVE));
	if (code < 0)
		return fail("keep-alive interval");

	// The tri-states stay untouched when the user left them on Automatic, which is what
	// keeps the backend free to pick.
	bool toggle = false;
	if (TriState(settings, QUIC_GSO, &toggle)) {
		code = moq_client_set_quic_gso(client, toggle);
		if (code < 0)
			return fail("UDP segmentation offload");
	}

	if (TriState(settings, QUIC_MTU_DISCOVERY, &toggle)) {
		code = moq_client_set_quic_mtu_discovery(client, toggle);
		if (code < 0)
			return fail("path MTU discovery");
	}

	const char *congestion = OptionalString(settings, QUIC_CONGESTION_CONTROL);
	code = moq_client_set_quic_congestion_control(client, congestion, congestion ? strlen(congestion) : 0);
	if (code < 0)
		return fail("congestion control");

	// A scene collection written by a qlog-capable build keeps the key even where the
	// field is hidden, and applying it there would fail the dial for a setting the user
	// can no longer see, let alone clear.
	const char *qlog = moq_qlog_supported() ? OptionalString(settings, QUIC_QLOG) : nullptr;
	code = moq_client_set_quic_qlog(client, qlog, qlog ? strlen(qlog) : 0);
	if (code < 0)
		return fail("qlog directory");

	code = moq_client_set_websocket_enabled(client, obs_data_get_bool(settings, WEBSOCKET_ENABLED));
	if (code < 0)
		return fail("WebSocket fallback");

	code = moq_client_set_websocket_delay(client, Amount(settings, WEBSOCKET_DELAY));
	if (code < 0)
		return fail("WebSocket fallback delay");

	return client;
}

} // namespace MoQSettings
