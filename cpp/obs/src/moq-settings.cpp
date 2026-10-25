// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-settings.h"
#include "logger.h"

#include <chrono>
#include <cstring>
#include <optional>
#include <string>
#include <vector>

namespace MoQSettings {

namespace {

// Keys. These are stable: they land in scene collections and in the dock's settings
// file, so renaming one silently drops whatever the user had configured.
constexpr const char *VERSION = "version";
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
constexpr const char *QUIC_KEYLOG = "quic_keylog";

constexpr const char *WEBSOCKET_ENABLED = "websocket_enabled";
constexpr const char *WEBSOCKET_DELAY = "websocket_delay_ms";

// Read a string setting, returning nullptr when it's unset or empty, which leaves the
// knob at the library default.
const char *OptionalString(obs_data_t *settings, const char *key)
{
	const char *value = obs_data_get_string(settings, key);
	return (value && *value) ? value : nullptr;
}

// Keep the first reason a setting can't be applied, so Configure reports the earliest.
void Refuse(std::string &error, std::string reason)
{
	if (error.empty())
		error = std::move(reason);
}

// Resolve a tri-state choice (AUTO / "on" / "off") into an override, or nothing to leave
// the choice to the transport. Any other stored value is refused.
std::optional<bool> TriState(obs_data_t *settings, const char *key, std::string &error)
{
	const char *value = OptionalString(settings, key);
	if (!value)
		return std::nullopt;
	if (strcmp(value, "on") == 0)
		return true;
	if (strcmp(value, "off") == 0)
		return false;
	Refuse(error, std::string("unknown ") + key + ": " + value);
	return std::nullopt;
}

// A library default as the milliseconds the UI edits. moq-ffi reports every duration
// this table shows; one it stops reporting reads as zero and is logged.
template<typename Duration> long long Millis(const std::optional<Duration> &duration, const char *key)
{
	if (!duration) {
		LOG_ERROR("moq-ffi reported no default for %s", key);
		return 0;
	}
	return static_cast<long long>(std::chrono::duration_cast<std::chrono::milliseconds>(*duration).count());
}

// Read an integer setting, refusing one outside the range its Field declares. Scene
// collections are editable JSON, so the widget bounds alone do not protect the unsigned
// API, and clamping would turn a malformed value into a different setting: a negative
// timeout into "wait forever".
uint64_t Amount(obs_data_t *settings, const char *key, std::string &error)
{
	const long long value = obs_data_get_int(settings, key);
	for (const Field &field : Fields()) {
		if (strcmp(field.key, key) != 0)
			continue;
		if (value < field.min || value > field.max) {
			Refuse(error, std::string(key) + " out of range: " + std::to_string(value));
			return 0;
		}
		return static_cast<uint64_t>(value);
	}

	Refuse(error, std::string("advanced integer setting has no field: ") + key);
	return 0;
}

// Read a millisecond setting through Amount.
std::chrono::milliseconds Milliseconds(obs_data_t *settings, const char *key, std::string &error)
{
	return std::chrono::milliseconds(static_cast<std::chrono::milliseconds::rep>(Amount(settings, key, error)));
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

// The tri-state used wherever the library default depends on the transport.
std::vector<Option> AutoOnOff()
{
	return {{"Automatic", AUTO}, {"Enabled", "on"}, {"Disabled", "off"}};
}

// The protocol versions this build offers, so the menu can't drift from what a dial
// accepts. Option borrows its pointers, so the names live as long as the field table.
std::vector<Option> VersionOptions()
{
	static const std::vector<std::string> names = moq::versions();

	std::vector<Option> options = {{"Automatic (offer all)", AUTO}};
	for (const std::string &name : names)
		options.push_back({name.c_str(), name.c_str()});
	return options;
}

} // namespace

const std::vector<Field> &Fields()
{
	static const std::vector<Field> fields = [] {
		// The library's own defaults, so the table doesn't repeat numbers that drift the
		// moment one is retuned.
		const moq::ClientConfig d = moq::client_defaults();
		std::vector<Field> f;

		// Protocol. Editable so a work-in-progress draft, which is never offered by
		// default and so never listed, can still be typed in.
		f.push_back(Choose(VERSION, "Protocol version", VersionOptions(), true,
				   "Pin the handshake to one draft instead of offering every supported version."));
		f.push_back(Text(BIND, "Bind address",
				 "Local UDP address to send from, e.g. 192.0.2.7:0 to pin the outgoing "
				 "interface. Leave empty for any."));
		f.push_back(Number(CONNECT_TIMEOUT, "Connect timeout (ms)", Millis(d.timeout, CONNECT_TIMEOUT), 0,
				   600000, 1000,
				   "How long one connection attempt may take, dial and handshake together. "
				   "0 waits forever."));
		f.push_back(Number(FAILOVER_DELAY, "Happy Eyeballs delay (ms)", Millis(d.race, FAILOVER_DELAY), 0,
				   10000, 50, "How long to wait before also trying the next address DNS returned."));

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
		f.push_back(Number(BACKOFF_INITIAL, "Reconnect delay (ms)",
				   Millis(d.backoff.initial, BACKOFF_INITIAL), 1, 60000, 100,
				   "Delay before the first reconnect attempt; it grows from here."));
		f.push_back(Number(BACKOFF_MAX, "Reconnect delay cap (ms)", Millis(d.backoff.max, BACKOFF_MAX), 1,
				   600000, 1000, "Ceiling on the growing reconnect delay."));
		f.push_back(Number(BACKOFF_TIMEOUT, "Give up after (ms)", Millis(d.backoff.timeout, BACKOFF_TIMEOUT),
				   0, 3600000, 1000,
				   "Total time to keep retrying before the stream fails. 0 retries forever."));

		// QUIC transport.
		f.push_back(Choose(
			QUIC_CONGESTION_CONTROL, "Congestion control",
			{{"Automatic", AUTO}, {"Delay-based (BBR)", "delay"}, {"Loss-based (CUBIC)", "loss"}}, false,
			"Delay-based keeps queues short and the send rate steady enough for an "
			"encoder to track. Loss-based chases throughput instead."));
		f.push_back(Number(QUIC_MAX_STREAMS, "Max concurrent streams",
				   static_cast<long long>(d.quic.max_streams.value_or(0)), 1, 65536, 1,
				   "MoQ opens a stream per group, so a busy publisher wants this high."));
		f.push_back(Number(QUIC_IDLE_TIMEOUT, "Idle timeout (ms)",
				   Millis(d.quic.idle_timeout, QUIC_IDLE_TIMEOUT), 0, 600000, 1000,
				   "How long an idle connection survives before it is dropped."));
		f.push_back(Number(QUIC_KEEP_ALIVE, "Keep-alive interval (ms)",
				   Millis(d.quic.keep_alive, QUIC_KEEP_ALIVE), 0, 60000, 100,
				   "0 disables the keep-alive pings."));
		f.push_back(Choose(QUIC_GSO, "UDP segmentation offload", AutoOnOff(), false,
				   "Batches sends into one syscall. Turn it off if large sends vanish; some "
				   "NICs and middleboxes mangle segmented packets."));
		f.push_back(Choose(QUIC_MTU_DISCOVERY, "Path MTU discovery", AutoOnOff(), false,
				   "Probe the path so packets use the largest size it allows. Turn it "
				   "off if discovery itself causes drops."));
		// Only when this build can capture: setting a directory without support fails
		// the dial, so offering it would be a control that only breaks things.
		if (moq::qlog_supported()) {
			f.push_back(Directory(QUIC_QLOG, "qlog directory",
					      "Write QUIC connection traces here for diagnosing stalls. Leave "
					      "empty to disable; the files get large."));
		}
		f.push_back(Toggle(QUIC_KEYLOG, "TLS key log", false,
				   "Write TLS session secrets to the file the SSLKEYLOGFILE environment "
				   "variable names, to decrypt a packet capture. Anyone holding that file "
				   "can read the stream. Going live fails if SSLKEYLOGFILE is unset."));

		// WebSocket fallback.
		f.push_back(Toggle(WEBSOCKET_ENABLED, "WebSocket fallback", d.websocket.enabled.value_or(true),
				   "Race a WebSocket connection against QUIC so a network that blocks UDP "
				   "still goes live. Turn it off to measure the QUIC path alone."));
		f.push_back(Number(WEBSOCKET_DELAY, "WebSocket fallback delay (ms)",
				   Millis(d.websocket.delay, WEBSOCKET_DELAY), 0, 10000, 50,
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

std::string Configure(obs_data_t *settings, moq::ClientConfig &config)
{
	if (!settings || !obs_data_get_bool(settings, ENABLED))
		return {}; // advanced off: the client keeps the library defaults

	std::string error;

	if (const char *version = OptionalString(settings, VERSION))
		config.versions = {version};
	if (const char *bind = OptionalString(settings, BIND))
		config.bind = bind;
	config.timeout = Milliseconds(settings, CONNECT_TIMEOUT, error);
	config.race = Milliseconds(settings, FAILOVER_DELAY, error);

	if (obs_data_get_bool(settings, TLS_DISABLE_VERIFY))
		config.tls.insecure = true;
	if (const char *fingerprint = OptionalString(settings, TLS_FINGERPRINT))
		config.tls.fingerprints = {fingerprint};
	if (const char *root = OptionalString(settings, TLS_ROOT))
		config.tls.roots = {root};
	if (const char *host_name = OptionalString(settings, TLS_HOST_NAME))
		config.tls.host_name = host_name;

	config.backoff.initial = Milliseconds(settings, BACKOFF_INITIAL, error);
	config.backoff.max = Milliseconds(settings, BACKOFF_MAX, error);
	config.backoff.timeout = Milliseconds(settings, BACKOFF_TIMEOUT, error);

	// Automatic leaves the tri-states and the congestion family unset, which keeps the
	// transport free to pick.
	if (const char *family = OptionalString(settings, QUIC_CONGESTION_CONTROL)) {
		if (strcmp(family, "delay") == 0)
			config.quic.congestion_control = moq::CongestionControl::kDelay;
		else if (strcmp(family, "loss") == 0)
			config.quic.congestion_control = moq::CongestionControl::kLoss;
		else
			Refuse(error, std::string("unknown congestion control: ") + family);
	}
	config.quic.max_streams = Amount(settings, QUIC_MAX_STREAMS, error);
	config.quic.idle_timeout = Milliseconds(settings, QUIC_IDLE_TIMEOUT, error);
	config.quic.keep_alive = Milliseconds(settings, QUIC_KEEP_ALIVE, error);
	config.quic.gso = TriState(settings, QUIC_GSO, error);
	config.quic.mtu_discovery = TriState(settings, QUIC_MTU_DISCOVERY, error);
	// A scene collection written by a qlog-capable build keeps the key even where the
	// field is hidden. Refusing would block Go Live on a setting the user can no longer
	// see, let alone clear, so the trace is skipped and the log says why.
	if (const char *qlog = OptionalString(settings, QUIC_QLOG)) {
		if (moq::qlog_supported())
			config.quic.qlog = qlog;
		else
			LOG_WARNING("Skipping the qlog directory %s: this build cannot write qlog", qlog);
	}
	config.quic.keylog = obs_data_get_bool(settings, QUIC_KEYLOG);

	config.websocket.enabled = obs_data_get_bool(settings, WEBSOCKET_ENABLED);
	config.websocket.delay = Milliseconds(settings, WEBSOCKET_DELAY, error);
	return error;
}

} // namespace MoQSettings
