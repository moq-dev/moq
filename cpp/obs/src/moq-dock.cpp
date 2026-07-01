// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-dock.h"
#include "logger.h"

#include <obs-module.h>
#include <obs-frontend-api.h>
#include <util/config-file.h>

#include <QFormLayout>
#include <QVBoxLayout>
#include <QLineEdit>
#include <QPushButton>
#include <QLabel>
#include <QFont>
#include <QTimer>
#include <QDir>
#include <QFileInfo>
#include <QMetaObject>

#include <cstring>
#include <random>
#include <string>

#ifndef MOQ_VERSION_STRING
#define MOQ_VERSION_STRING "unknown"
#endif

namespace {

// Map OBS's "simple output" encoder aliases to real encoder ids, mirroring the
// table OBS uses internally. Falls back to x264 for anything unrecognized.
const char *SimpleVideoEncoderId(const char *name)
{
	if (!name)
		return "obs_x264";
	if (strcmp(name, "x264") == 0 || strcmp(name, "x264_lowcpu") == 0)
		return "obs_x264";
	if (strcmp(name, "qsv") == 0)
		return "obs_qsv11_v2";
	if (strcmp(name, "qsv_av1") == 0)
		return "obs_qsv11_av1_v2";
	if (strcmp(name, "amd") == 0)
		return "h264_texture_amf";
	if (strcmp(name, "amd_hevc") == 0)
		return "h265_texture_amf";
	if (strcmp(name, "amd_av1") == 0)
		return "av1_texture_amf";
	if (strcmp(name, "nvenc") == 0)
		return "obs_nvenc_h264_tex";
	if (strcmp(name, "nvenc_hevc") == 0)
		return "obs_nvenc_hevc_tex";
	if (strcmp(name, "nvenc_av1") == 0)
		return "obs_nvenc_av1_tex";
	if (strcmp(name, "apple_h264") == 0)
		return "com.apple.videotoolbox.videoencoder.ave.avc";
	if (strcmp(name, "apple_hevc") == 0)
		return "com.apple.videotoolbox.videoencoder.ave.hevc";
	return "obs_x264";
}

const char *SimpleAudioEncoderId(const char *name)
{
	if (name && strcmp(name, "opus") == 0)
		return "ffmpeg_opus";
	return "ffmpeg_aac";
}

std::string SettingsPath()
{
	char *p = obs_module_config_path("dock.json");
	std::string s = p ? p : "";
	bfree(p);
	return s;
}

// Default broadcast name "obs-<rand>" so distinct setups don't collide on a
// shared relay out of the box. Only used until the user edits/saves their own.
std::string RandomBroadcastName()
{
	static const char charset[] = "abcdefghijklmnopqrstuvwxyz0123456789";
	std::random_device rd;
	std::mt19937 gen(rd());
	std::uniform_int_distribution<int> dist(0, (int)sizeof(charset) - 2);
	std::string s = "obs-";
	for (int i = 0; i < 6; i++)
		s += charset[dist(gen)];
	return s;
}

} // namespace

MoQDock::MoQDock(QWidget *parent) : QWidget(parent)
{
	urlEdit = new QLineEdit(this);
	urlEdit->setText("https://cdn.moq.dev/anon");
	urlEdit->setPlaceholderText("http://localhost:4443/anon");

	pathEdit = new QLineEdit(this);
	pathEdit->setText(QString::fromStdString(RandomBroadcastName()));
	pathEdit->setPlaceholderText("(optional) broadcast name");

	// Labels above the fields (WrapAllRows), and let the fields grow to the full
	// dock width (the macOS default keeps them at their size hint otherwise).
	auto *form = new QFormLayout();
	form->setRowWrapPolicy(QFormLayout::WrapAllRows);
	form->setFieldGrowthPolicy(QFormLayout::AllNonFixedFieldsGrow);
	form->setContentsMargins(0, 0, 0, 0);
	form->addRow("Relay URL", urlEdit);
	form->addRow("Broadcast name", pathEdit);

	button = new QPushButton("Go Live", this);
	button->setCursor(Qt::PointingHandCursor);
	connect(button, &QPushButton::clicked, this, &MoQDock::ToggleStream);

	status = new QLabel(this);
	status->setWordWrap(true);
	QFont statusFont = status->font();
	statusFont.setBold(true);
	status->setFont(statusFont);

	auto *versionLabel = new QLabel(QString("libmoq %1").arg(MOQ_VERSION_STRING), this);
	versionLabel->setAlignment(Qt::AlignRight | Qt::AlignBottom);
	versionLabel->setStyleSheet("color: #888888; font-size: 10px;");

	auto *layout = new QVBoxLayout(this);
	layout->setSpacing(10);
	layout->addLayout(form);
	layout->addWidget(button);
	layout->addWidget(status);
	layout->addStretch();
	layout->addWidget(versionLabel);

	pollTimer = new QTimer(this);
	pollTimer->setInterval(1000);
	connect(pollTimer, &QTimer::timeout, this, &MoQDock::UpdateStatus);

	connect(urlEdit, &QLineEdit::editingFinished, this, &MoQDock::SaveSettings);
	connect(pathEdit, &QLineEdit::editingFinished, this, &MoQDock::SaveSettings);

	LoadSettings();
	SetRunning(false);
}

MoQDock::~MoQDock()
{
	StopStream();
}

void MoQDock::ToggleStream()
{
	if (running) {
		StopStream();
	} else {
		StartStream();
	}
}

bool MoQDock::CreateConfiguredEncoders()
{
	config_t *config = obs_frontend_get_profile_config();
	if (!config) {
		LOG_ERROR("No profile config available");
		return false;
	}

	const char *mode = config_get_string(config, "Output", "Mode");
	const bool advanced = mode && strcmp(mode, "Advanced") == 0;

	OBSDataAutoRelease videoSettings = obs_data_create();
	OBSDataAutoRelease audioSettings = obs_data_create();
	const char *videoId = nullptr;
	const char *audioId = nullptr;
	int audioBitrate = 0;
	size_t audioMixerIdx = 0;

	if (advanced) {
		videoId = config_get_string(config, "AdvOut", "Encoder");

		// Advanced video encoder settings live in a JSON file in the profile dir.
		char *profilePath = obs_frontend_get_current_profile_path();
		if (profilePath) {
			std::string file = std::string(profilePath) + "/streamEncoder.json";
			bfree(profilePath);
			OBSDataAutoRelease loaded = obs_data_create_from_json_file(file.c_str());
			if (loaded)
				obs_data_apply(videoSettings, loaded);
		}

		audioId = config_get_string(config, "AdvOut", "AudioEncoder");
		int track = (int)config_get_int(config, "AdvOut", "TrackIndex");
		if (track < 1)
			track = 1;
		// OBS config tracks are 1-based; libobs mixer indices are 0-based.
		audioMixerIdx = (size_t)(track - 1);
		char key[32];
		snprintf(key, sizeof(key), "Track%dBitrate", track);
		audioBitrate = (int)config_get_int(config, "AdvOut", key);
	} else {
		videoId = SimpleVideoEncoderId(config_get_string(config, "SimpleOutput", "StreamEncoder"));
		int videoBitrate = (int)config_get_int(config, "SimpleOutput", "VBitrate");
		if (videoBitrate <= 0)
			videoBitrate = 2500;
		obs_data_set_int(videoSettings, "bitrate", videoBitrate);
		obs_data_set_string(videoSettings, "rate_control", "CBR");
		const char *preset = config_get_string(config, "SimpleOutput", "Preset");
		if (preset)
			obs_data_set_string(videoSettings, "preset", preset);

		audioId = SimpleAudioEncoderId(config_get_string(config, "SimpleOutput", "StreamAudioEncoder"));
		audioBitrate = (int)config_get_int(config, "SimpleOutput", "ABitrate");
	}

	if (!videoId || !*videoId)
		videoId = "obs_x264";
	if (!audioId || !*audioId)
		audioId = "ffmpeg_aac";
	if (audioBitrate <= 0)
		audioBitrate = 160;

	// MoQ publishes inline headers (avc3/hev1), so force repeat_headers and no
	// B-frames, mirroring MoQService::ApplyEncoderSettings.
	obs_data_set_bool(videoSettings, "repeat_headers", true);
	obs_data_set_int(videoSettings, "bf", 0);
	obs_data_set_int(audioSettings, "bitrate", audioBitrate);

	videoEncoder =
		OBSEncoderAutoRelease(obs_video_encoder_create(videoId, "moq_dock_video", videoSettings, nullptr));
	audioEncoder = OBSEncoderAutoRelease(
		obs_audio_encoder_create(audioId, "moq_dock_audio", audioSettings, audioMixerIdx, nullptr));
	if (!videoEncoder || !audioEncoder) {
		LOG_ERROR("Failed to create encoders (%s / %s)", videoId, audioId);
		return false;
	}

	obs_encoder_set_video(videoEncoder, obs_get_video());
	obs_encoder_set_audio(audioEncoder, obs_get_audio());

	LOG_INFO("Using configured stream encoders: %s / %s", videoId, audioId);
	return true;
}

void MoQDock::StartStream()
{
	const std::string url = urlEdit->text().toStdString();
	const std::string path = pathEdit->text().toStdString();
	if (url.empty()) {
		status->setText("Relay URL is required");
		return;
	}

	SaveSettings();

	// The MoQ output reads the server URL / path from its attached service, so
	// build a throwaway service from the dock fields.
	OBSDataAutoRelease serviceSettings = obs_data_create();
	obs_data_set_string(serviceSettings, "server", url.c_str());
	obs_data_set_string(serviceSettings, "key", path.c_str());
	service =
		OBSServiceAutoRelease(obs_service_create("moq_service", "moq_dock_service", serviceSettings, nullptr));
	if (!service) {
		status->setText("Failed to create service");
		return;
	}

	if (!CreateConfiguredEncoders()) {
		status->setText("Failed to set up encoders");
		return;
	}

	output = OBSOutputAutoRelease(obs_output_create("moq_output", "moq_dock_output", nullptr, nullptr));
	if (!output) {
		status->setText("Failed to create output");
		return;
	}

	obs_output_set_service(output, service);
	obs_output_set_video_encoder(output, videoEncoder);
	obs_output_set_audio_encoder(output, audioEncoder, 0);

	signal_handler_connect(obs_output_get_signal_handler(output), "stop", OnOutputStopped, this);

	if (!obs_output_start(output)) {
		const char *err = obs_output_get_last_error(output);
		status->setText(err ? QString("Failed to start: %1").arg(err) : "Failed to start");
		LOG_ERROR("Failed to start MoQ dock output: %s", err ? err : "(no error)");
		StopStream();
		return;
	}

	pollTimer->start();

	SetRunning(true);
	status->setText("● Connecting…");
	status->setStyleSheet("color: #d08b1d;");
}

void MoQDock::StopStream()
{
	pollTimer->stop();

	if (output) {
		signal_handler_disconnect(obs_output_get_signal_handler(output), "stop", OnOutputStopped, this);
		obs_output_stop(output);
	}

	output = nullptr;
	service = nullptr;
	videoEncoder = nullptr;
	audioEncoder = nullptr;

	SetRunning(false);
}

void MoQDock::SetRunning(bool isRunning)
{
	running = isRunning;

	button->setText(isRunning ? "Stop" : "Go Live");
	button->setStyleSheet(QString("QPushButton { padding: 8px; border-radius: 4px; font-weight: bold; "
				      "color: white; background-color: %1; }"
				      "QPushButton:hover { background-color: %2; }")
				      .arg(isRunning ? "#c0392b" : "#2d8a4e")
				      .arg(isRunning ? "#e04434" : "#36a45e"));

	urlEdit->setEnabled(!isRunning);
	pathEdit->setEnabled(!isRunning);

	if (!isRunning) {
		status->setText("● Disconnected");
		status->setStyleSheet("color: #888888;");
	}
}

void MoQDock::UpdateStatus()
{
	if (!output || !running)
		return;

	// libmoq surfaces connection state via the session-connect callback, which
	// MoQOutput records as the output's connect time; until that fires we're
	// still connecting. There's no per-frame stats API to show beyond this.
	const bool connected = obs_output_get_connect_time_ms(output) > 0;
	status->setText(connected ? "● Connected" : "● Connecting…");
	status->setStyleSheet(connected ? "color: #36a45e;" : "color: #d08b1d;");
}

void MoQDock::LoadSettings()
{
	const std::string path = SettingsPath();
	if (path.empty())
		return;

	OBSDataAutoRelease data = obs_data_create_from_json_file(path.c_str());
	if (!data)
		return;

	const char *url = obs_data_get_string(data, "url");
	const char *broadcast = obs_data_get_string(data, "path");
	if (url && *url)
		urlEdit->setText(url);
	if (obs_data_has_user_value(data, "path"))
		pathEdit->setText(broadcast ? broadcast : "");
}

void MoQDock::SaveSettings()
{
	const std::string path = SettingsPath();
	if (path.empty())
		return;

	QDir().mkpath(QFileInfo(QString::fromStdString(path)).absolutePath());

	OBSDataAutoRelease data = obs_data_create();
	obs_data_set_string(data, "url", urlEdit->text().toUtf8().constData());
	obs_data_set_string(data, "path", pathEdit->text().toUtf8().constData());
	obs_data_save_json(data, path.c_str());
}

void MoQDock::OnOutputStopped(void *data, calldata_t *params)
{
	auto *self = static_cast<MoQDock *>(data);
	long long code = calldata_int(params, "code");

	// Signals arrive on an OBS thread; bounce to the Qt thread before touching widgets.
	QMetaObject::invokeMethod(
		self,
		[self, code]() {
			// StopStream() resets the status to "Idle", so set the failure
			// message afterwards or it would be immediately overwritten.
			self->StopStream();
			if (code != OBS_OUTPUT_SUCCESS)
				self->status->setText(QString("Stopped (code %1)").arg(code));
		},
		Qt::QueuedConnection);
}

void register_moq_dock()
{
	// OBS takes ownership of the widget; create it without a parent.
	auto *dock = new MoQDock();
	obs_frontend_add_dock_by_id("moq_dock", "MoQ", dock);
}
