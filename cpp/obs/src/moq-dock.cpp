// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-dock.h"
#include "moq-advanced-dialog.h"
#include "moq-output.h"
#include "moq-quality-defaults.h"
#include "moq-settings.h"
#include "logger.h"

#include <obs-module.h>
#include <obs-frontend-api.h>
#include <util/config-file.h>

#include <QCheckBox>
#include <QComboBox>
#include <QCoreApplication>
#include <QFormLayout>
#include <QVBoxLayout>
#include <QLineEdit>
#include <QPushButton>
#include <QLabel>
#include <QGroupBox>
#include <QFont>
#include <QTimer>
#include <QDir>
#include <QFileInfo>
#include <QMetaObject>
#include <QPointer>
#include <QSignalBlocker>
#include <QStringList>
#include <QTabWidget>
#include <QUrl>
#include <QUrlQuery>
#include <QPainter>
#include <QPainterPath>
#include <QPen>
#include <QColor>
#include <QPaintEvent>
#include <QElapsedTimer>

#include <algorithm>
#include <cstring>
#include <deque>
#include <random>
#include <string>
#include <vector>

#ifndef MOQ_VERSION_STRING
#define MOQ_VERSION_STRING "unknown"
#endif
#ifndef PLUGIN_VERSION_STRING
#define PLUGIN_VERSION_STRING "unknown"
#endif

namespace {

struct EncoderOffer {
	QString id;
	QString display;
	QString codec; // h264, hevc, av1, aac, opus, …
	bool hardware = false;
	bool video = true;
};

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
	// Hang convention: a .hang suffix marks a media broadcast.
	s += ".hang";
	return s;
}

QString FormatBitrate(double bps)
{
	if (bps >= 1'000'000.0)
		return QString("%1 Mbps").arg(bps / 1'000'000.0, 0, 'f', 1);
	if (bps >= 1'000.0)
		return QString("%1 kbps").arg(bps / 1'000.0, 0, 'f', 0);
	return QString("%1 bps").arg(bps, 0, 'f', 0);
}

QString FormatBytes(uint64_t bytes)
{
	if (bytes >= 1'000'000'000ULL)
		return QString("%1 GB").arg(bytes / 1'000'000'000.0, 0, 'f', 1);
	if (bytes >= 1'000'000ULL)
		return QString("%1 MB").arg(bytes / 1'000'000.0, 0, 'f', 1);
	if (bytes >= 1'000ULL)
		return QString("%1 KB").arg(bytes / 1'000.0, 0, 'f', 0);
	return QString("%1 B").arg(bytes);
}

static QString ExtractTimeout(const QString &raw)
{
	const int at = raw.indexOf(QStringLiteral("after "));
	if (at < 0)
		return {};
	QString rest = raw.mid(at + 6);
	int end = 0;
	while (end < rest.size() && (rest[end].isDigit() || rest[end] == QLatin1Char('.')))
		end++;
	if (end == 0)
		return {};
	QString value = rest.left(end);
	if (rest.mid(end).startsWith(QLatin1Char('s')))
		return value + QStringLiteral("s");
	return value;
}

// Short status-line copy for the dock. Prefer the innermost cause (unauthorized)
// over the reconnect wrapper so the publisher knows what to fix.
QString ExplainFailure(int code, const std::string &reason)
{
	const QString raw = QString::fromUtf8(reason.c_str());
	const QString lower = raw.toLower();
	const QString timeout = ExtractTimeout(lower);

	if (lower.contains(QStringLiteral("unauthorized")) || code == -34) {
		QString text = QStringLiteral("Unauthorized · relay rejected this URL or token");
		if (!timeout.isEmpty())
			text += QStringLiteral(" · gave up after ") + timeout;
		return text;
	}
	if (lower.contains(QStringLiteral("forbidden")) || code == -35)
		return QStringLiteral("Forbidden · this broadcast path is not allowed");
	if (lower.contains(QStringLiteral("fingerprint")) || lower.contains(QStringLiteral("certificate")))
		return QStringLiteral("Could not trust the relay certificate");
	if (lower.contains(QStringLiteral("timed out")) || lower.contains(QStringLiteral("timeout"))) {
		QString text = QStringLiteral("Timed out reaching the relay");
		if (!timeout.isEmpty())
			text += QStringLiteral(" · gave up after ") + timeout;
		return text;
	}
	if (lower.contains(QStringLiteral("failed to connect")) || lower.contains(QStringLiteral("dns")))
		return QStringLiteral("Could not reach the relay · check the URL and network");
	if (code == -5 || lower.contains(QStringLiteral("connect error")))
		return QStringLiteral("Connect failed · the relay did not accept the session");
	if (code == -17 || lower == QStringLiteral("offline"))
		return {};
	if (!raw.isEmpty())
		return raw;
	return {};
}

QString NormalizeCodec(const char *codec)
{
	if (!codec || !*codec)
		return {};
	QString c = QString::fromUtf8(codec).toLower();
	if (c == "h264" || c == "avc")
		return "h264";
	if (c == "h265" || c == "hevc")
		return "hevc";
	if (c == "av1")
		return "av1";
	if (c == "aac")
		return "aac";
	if (c == "opus")
		return "opus";
	return c;
}

bool LooksHardware(const char *id, uint32_t caps)
{
	if (caps & OBS_ENCODER_CAP_PASS_TEXTURE)
		return true;
	if (!id)
		return false;
	QString s = QString::fromUtf8(id).toLower();
	return s.contains("nvenc") || s.contains("qsv") || s.contains("amf") || s.contains("videotoolbox") ||
	       s.contains("apple") || s.contains("vaapi") || s.contains("mfx");
}

std::vector<EncoderOffer> EnumerateEncoders()
{
	std::vector<EncoderOffer> out;
	const char *id = nullptr;
	for (size_t i = 0; obs_enum_encoder_types(i, &id); i++) {
		if (!id || !*id)
			continue;
		const uint32_t caps = obs_get_encoder_caps(id);
		if (caps & OBS_ENCODER_CAP_DEPRECATED)
			continue;
		if (caps & OBS_ENCODER_CAP_INTERNAL)
			continue;

		const enum obs_encoder_type type = obs_get_encoder_type(id);
		const bool video = type == OBS_ENCODER_VIDEO;
		const bool audio = type == OBS_ENCODER_AUDIO;
		if (!video && !audio)
			continue;

		const QString codec = NormalizeCodec(obs_get_encoder_codec(id));
		if (video && codec != "h264" && codec != "hevc" && codec != "av1")
			continue;
		if (audio && codec != "aac" && codec != "opus")
			continue;

		EncoderOffer offer;
		offer.id = QString::fromUtf8(id);
		const char *display = obs_encoder_get_display_name(id);
		offer.display = display && *display ? QString::fromUtf8(display) : offer.id;
		offer.codec = codec;
		offer.hardware = video && LooksHardware(id, caps);
		offer.video = video;
		out.push_back(std::move(offer));
	}
	return out;
}

void SelectComboData(QComboBox *combo, const QString &data)
{
	const int idx = combo->findData(data);
	if (idx >= 0)
		combo->setCurrentIndex(idx);
}

} // namespace

enum class SparkUnit { Bitrate, Millis, Percent, Bytes };

QString FormatSpark(SparkUnit unit, double value)
{
	switch (unit) {
	case SparkUnit::Bitrate:
		return FormatBitrate(value);
	case SparkUnit::Millis:
		return QString("%1 ms").arg(value, 0, 'f', value < 10.0 ? 1 : 0);
	case SparkUnit::Percent:
		return QString("%1%").arg(value, 0, 'f', value < 10.0 ? 1 : 0);
	case SparkUnit::Bytes:
		return FormatBytes(static_cast<uint64_t>(value < 0 ? 0 : value));
	}
	return {};
}

// One compact sparkline row: title, 60s plot, current value.
class MoQSpark : public QWidget {
public:
	MoQSpark(const QString &title, SparkUnit unit, QColor color, QWidget *parent = nullptr)
		: QWidget(parent),
		  title_(title),
		  unit_(unit),
		  color_(color)
	{
		setMinimumHeight(28);
		setMaximumHeight(32);
		setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Fixed);
		setToolTip(title);
	}

	void Clear()
	{
		samples_.clear();
		latest_ = 0;
		have_ = false;
		update();
	}

	void Push(bool valid, double value)
	{
		Sample s;
		s.valid = valid;
		s.value = value;
		samples_.push_back(s);
		while (samples_.size() > 60)
			samples_.pop_front();
		if (valid) {
			have_ = true;
			latest_ = value;
		}
		update();
	}

protected:
	void paintEvent(QPaintEvent *) override
	{
		QPainter p(this);
		p.setRenderHint(QPainter::Antialiasing);
		p.fillRect(rect(), QColor("#1c1c1c"));
		p.setPen(QColor("#3a3a3a"));
		p.drawRect(rect().adjusted(0, 0, -1, -1));

		const int labelW = 36;
		const int valueW = 58;
		const QRect plot = rect().adjusted(labelW + 4, 5, -(valueW + 4), -5);

		p.setPen(QColor("#a8a8a8"));
		QFont f = font();
		f.setPointSize(9);
		p.setFont(f);
		p.drawText(QRect(6, 0, labelW, height()), Qt::AlignVCenter | Qt::AlignLeft, title_);

		size_t validCount = 0;
		for (const Sample &s : samples_) {
			if (s.valid)
				validCount++;
		}
		if (validCount >= 2 && plot.width() > 8 && plot.height() > 4) {
			double lo = 0;
			double hi = 0;
			bool haveRange = false;
			for (const Sample &s : samples_) {
				if (!s.valid)
					continue;
				if (!haveRange) {
					lo = hi = s.value;
					haveRange = true;
				} else {
					lo = std::min(lo, s.value);
					hi = std::max(hi, s.value);
				}
			}
			if (unit_ != SparkUnit::Bytes)
				lo = 0;
			const double span = std::max(hi - lo, 1e-9);
			QPainterPath path;
			bool drawing = false;
			for (size_t i = 0; i < samples_.size(); i++) {
				if (!samples_[i].valid) {
					drawing = false;
					continue;
				}
				const double x = plot.left() + (plot.width() * static_cast<double>(i) /
								static_cast<double>(samples_.size() - 1));
				const double y = plot.bottom() - (plot.height() * ((samples_[i].value - lo) / span));
				if (!drawing) {
					path.moveTo(x, y);
					drawing = true;
				} else {
					path.lineTo(x, y);
				}
			}
			p.setPen(QPen(color_, 1.25));
			p.drawPath(path);
		}

		p.setPen(QColor("#e0e0e0"));
		f.setPointSize(9);
		p.setFont(f);
		const QString value = have_ ? FormatSpark(unit_, latest_) : QStringLiteral("-");
		p.drawText(QRect(width() - valueW - 4, 0, valueW, height()), Qt::AlignVCenter | Qt::AlignRight, value);
	}

private:
	struct Sample {
		bool valid = false;
		double value = 0;
	};

	QString title_;
	SparkUnit unit_;
	QColor color_;
	std::deque<Sample> samples_;
	double latest_ = 0;
	bool have_ = false;
};

MoQDock::MoQDock(QWidget *parent) : QWidget(parent)
{
	tabs = new QTabWidget(this);
	auto *streamPage = new QWidget(tabs);

	urlEdit = new QLineEdit(streamPage);
	urlEdit->setText("https://cdn.moq.dev/anon");
	urlEdit->setPlaceholderText("http://localhost:4443/anon");
	const QString urlHelp = "Relay origin URL, for example https://cdn.moq.dev/anon. "
				"Paste a URL with ?jwt= and the token is moved into Publish token.";
	urlEdit->setToolTip(urlHelp);

	tokenEdit = new QLineEdit(streamPage);
	tokenEdit->setPlaceholderText("(optional) publish JWT");
	tokenEdit->setEchoMode(QLineEdit::Password);
	const QString tokenHelp = "Optional relay publish token. Leave empty for public relays such as /anon. "
				  "Pasting a URL with ?jwt= into Relay URL fills this and strips it from the URL.";
	tokenEdit->setToolTip(tokenHelp);

	pathEdit = new QLineEdit(streamPage);
	pathEdit->setText(QString::fromStdString(RandomBroadcastName()));
	pathEdit->setPlaceholderText("(optional) broadcast name");
	const QString pathHelp = "Broadcast path on the relay. Viewers subscribe to this name. "
				 "A .hang suffix is the usual hang catalog convention.";
	pathEdit->setToolTip(pathHelp);

	auto *form = new QFormLayout();
	form->setRowWrapPolicy(QFormLayout::WrapAllRows);
	form->setFieldGrowthPolicy(QFormLayout::AllNonFixedFieldsGrow);
	form->setContentsMargins(0, 0, 0, 0);
	form->addRow(MoQHintLabel("Relay URL", urlHelp, streamPage), urlEdit);
	form->addRow(MoQHintLabel("Publish token", tokenHelp, streamPage), tokenEdit);
	form->addRow(MoQHintLabel("Broadcast name", pathHelp, streamPage), pathEdit);

	button = new QPushButton("Go Live", streamPage);
	button->setCursor(Qt::PointingHandCursor);
	button->setToolTip("Start or stop publishing this broadcast to the relay.");
	connect(button, &QPushButton::clicked, this, &MoQDock::ToggleStream);

	advancedButton = new QPushButton("Advanced…", streamPage);
	advancedButton->setCursor(Qt::PointingHandCursor);
	advancedButton->setToolTip("Protocol pin, TLS, reconnect, QUIC and WebSocket settings. "
				   "Opens them in a window. Changes apply on the next Go Live.");
	connect(advancedButton, &QPushButton::clicked, this, &MoQDock::OpenAdvanced);

	advanced = OBSDataAutoRelease(obs_data_create());
	MoQSettings::Defaults(advanced);

	status = new QLabel(streamPage);
	status->setWordWrap(true);
	status->setTextFormat(Qt::PlainText);
	status->setToolTip("Live session state: connecting, connected, reconnecting, or the last failure.");
	QFont statusFont = status->font();
	statusFont.setBold(true);
	status->setFont(statusFont);

	const QString statsHelp = "While live: Quality encode, negotiated MoQ draft and dial URL scheme. "
				  "RTT / bitrate / loss live in the timeline panels.";
	showStats = new QCheckBox("Show stats", streamPage);
	showStats->setChecked(true);
	showStats->setToolTip(statsHelp);
	connect(showStats, &QCheckBox::toggled, this, [this](bool) {
		SaveSettings();
		ApplyView();
	});

	statsBox = new QLabel(streamPage);
	statsBox->setWordWrap(true);
	statsBox->setTextFormat(Qt::PlainText);
	statsBox->setTextInteractionFlags(Qt::TextSelectableByMouse);
	statsBox->setMinimumHeight(40);
	statsBox->setAlignment(Qt::AlignTop | Qt::AlignLeft);
	statsBox->setStyleSheet("QLabel { color: #d0d0d0; font-size: 11px; background: #1c1c1c; "
				"border: 1px solid #464646; border-radius: 4px; padding: 6px; }");
	statsBox->setText("Waiting for the first connect.");

	statsBox->setToolTip(statsHelp);

	const QString timelineHelp =
		"One sparkline per live metric for the last minute: RTT, send, recv, loss, and bytes sent.";
	showTimeline = new QCheckBox("Show timeline", streamPage);
	showTimeline->setChecked(true);
	showTimeline->setToolTip(timelineHelp);
	connect(showTimeline, &QCheckBox::toggled, this, [this](bool) {
		SaveSettings();
		ApplyView();
	});

	sparkBox = new QWidget(streamPage);
	auto *sparkLayout = new QVBoxLayout(sparkBox);
	sparkLayout->setContentsMargins(0, 0, 0, 0);
	sparkLayout->setSpacing(3);
	rttSpark = new MoQSpark("RTT", SparkUnit::Millis, QColor("#6cb6ff"), sparkBox);
	sendSpark = new MoQSpark("Send", SparkUnit::Bitrate, QColor("#36a45e"), sparkBox);
	recvSpark = new MoQSpark("Recv", SparkUnit::Bitrate, QColor("#c9a227"), sparkBox);
	lossSpark = new MoQSpark("Loss", SparkUnit::Percent, QColor("#e07a5f"), sparkBox);
	sentSpark = new MoQSpark("Sent", SparkUnit::Bytes, QColor("#9b8ec4"), sparkBox);
	rttSpark->setToolTip("Smoothed round-trip time from the QUIC congestion controller.");
	sendSpark->setToolTip("Estimated send bandwidth.");
	recvSpark->setToolTip("Estimated receive bandwidth from MoQ PROBE, when the draft supports it.");
	lossSpark->setToolTip("Detected packet loss as a percent of packets sent.");
	sentSpark->setToolTip("Total bytes sent on the connection, including retransmits.");
	sparkLayout->addWidget(rttSpark);
	sparkLayout->addWidget(sendSpark);
	sparkLayout->addWidget(recvSpark);
	sparkLayout->addWidget(lossSpark);
	sparkLayout->addWidget(sentSpark);

	auto *streamLibmoq = new QLabel(streamPage);
	streamLibmoq->setAlignment(Qt::AlignRight | Qt::AlignBottom);
	streamLibmoq->setStyleSheet("color: #888888; font-size: 10px;");
	streamLibmoq->setOpenExternalLinks(true);
	streamLibmoq->setText(
		QString("<a href=\"https://doc.moq.dev/lib/c/\" style=\"color:#888888; text-decoration:none;\">libmoq %1</a>")
			.arg(MOQ_VERSION_STRING));
	streamLibmoq->setToolTip("Open libmoq docs on doc.moq.dev");

	auto *streamLayout = new QVBoxLayout(streamPage);
	streamLayout->setContentsMargins(0, 8, 0, 0);
	streamLayout->setSpacing(10);
	streamLayout->addLayout(form);
	streamLayout->addWidget(button);
	streamLayout->addWidget(advancedButton);
	streamLayout->addWidget(status);
	streamLayout->addWidget(showStats);
	streamLayout->addWidget(statsBox);
	streamLayout->addWidget(showTimeline);
	streamLayout->addWidget(sparkBox);
	streamLayout->addStretch();
	streamLayout->addWidget(streamLibmoq);

	auto *qualityPage = new QWidget(tabs);
	qualityToggle = new QCheckBox("Custom source quality", qualityPage);
	qualityToggle->setToolTip("What OBS encodes and publishes. Off uses Settings → Output. On uses "
				  "the Quality / Performance profile and codec picks below.");

	qualityBox = new QGroupBox("Source encode", qualityPage);
	auto *qForm = new QFormLayout(qualityBox);
	qForm->setRowWrapPolicy(QFormLayout::WrapAllRows);
	qForm->setFieldGrowthPolicy(QFormLayout::AllNonFixedFieldsGrow);

	profileCombo = new QComboBox(qualityBox);
	profileCombo->addItem("Auto (detected)", "auto");
	profileCombo->addItem("Quality", "high");
	profileCombo->addItem("Performance", "low");

	detectedLabel = new QLabel(qualityBox);
	detectedLabel->setWordWrap(true);
	detectedLabel->setStyleSheet("color: #888888;");

	pathCombo = new QComboBox(qualityBox);
	pathCombo->addItem("Hardware preferred", "hardware");
	pathCombo->addItem("Software preferred", "software");

	videoCodecCombo = new QComboBox(qualityBox);
	videoEncoderCombo = new QComboBox(qualityBox);
	audioCodecCombo = new QComboBox(qualityBox);

	qForm->addRow("Profile", profileCombo);
	qForm->addRow(detectedLabel);
	qForm->addRow("Encoder path", pathCombo);
	qForm->addRow("Video codec", videoCodecCombo);
	qForm->addRow("Video encoder", videoEncoderCombo);
	qForm->addRow("Audio codec", audioCodecCombo);

	auto *qualityNote = new QLabel(qualityPage);
	qualityNote->setWordWrap(true);
	qualityNote->setStyleSheet("color: #888888;");
	qualityNote->setText("While publishing, Stream → Show stats lists this encode, "
			     "plus protocol and dial URL scheme.");

	auto *qualityLayout = new QVBoxLayout(qualityPage);
	qualityLayout->setContentsMargins(0, 8, 0, 0);
	qualityLayout->setSpacing(10);
	qualityLayout->addWidget(qualityToggle);
	qualityLayout->addWidget(qualityBox);
	qualityLayout->addWidget(qualityNote);
	qualityLayout->addStretch();
	qualityBox->setEnabled(false);

	auto *versionPage = new QWidget(tabs);
	auto *verForm = new QFormLayout(versionPage);
	verForm->setRowWrapPolicy(QFormLayout::WrapAllRows);
	verForm->setFieldGrowthPolicy(QFormLayout::AllNonFixedFieldsGrow);
	verForm->setContentsMargins(0, 8, 0, 0);

	auto *pluginVer = new QLabel(versionPage);
	pluginVer->setOpenExternalLinks(true);
	pluginVer->setTextInteractionFlags(Qt::TextBrowserInteraction);
	pluginVer->setText(
		QString("<a href=\"https://doc.moq.dev/bin/obs\">%1</a>").arg(QString::fromUtf8(PLUGIN_VERSION_STRING)));
	pluginVer->setToolTip("OBS plugin docs on doc.moq.dev");

	auto *libmoqVer = new QLabel(versionPage);
	libmoqVer->setOpenExternalLinks(true);
	libmoqVer->setTextInteractionFlags(Qt::TextBrowserInteraction);
	libmoqVer->setText(
		QString("<a href=\"https://doc.moq.dev/lib/c/\">%1</a>").arg(QString::fromUtf8(MOQ_VERSION_STRING)));
	libmoqVer->setToolTip("libmoq C API docs on doc.moq.dev");

	auto *moqDevLink = new QLabel(versionPage);
	moqDevLink->setOpenExternalLinks(true);
	moqDevLink->setTextInteractionFlags(Qt::TextBrowserInteraction);
	moqDevLink->setText("<a href=\"https://moq.dev\">moq.dev</a>");

	auto *moqProLink = new QLabel(versionPage);
	moqProLink->setOpenExternalLinks(true);
	moqProLink->setTextInteractionFlags(Qt::TextBrowserInteraction);
	moqProLink->setText("<a href=\"https://moq.pro\">moq.pro</a>");

	verForm->addRow("Plugin", pluginVer);
	verForm->addRow("libmoq", libmoqVer);
	verForm->addRow("moq.dev", moqDevLink);
	verForm->addRow("moq.pro", moqProLink);

	tabs->addTab(streamPage, "Stream");
	tabs->addTab(qualityPage, "Quality");
	tabs->addTab(versionPage, "Version");

	auto *layout = new QVBoxLayout(this);
	layout->setContentsMargins(8, 8, 8, 8);
	layout->addWidget(tabs);

	pollTimer = new QTimer(this);
	pollTimer->setInterval(1000);
	connect(pollTimer, &QTimer::timeout, this, &MoQDock::UpdateStatus);

	connect(urlEdit, &QLineEdit::editingFinished, this, &MoQDock::OnRelayUrlEdited);
	connect(tokenEdit, &QLineEdit::editingFinished, this, &MoQDock::SaveSettings);
	connect(pathEdit, &QLineEdit::editingFinished, this, &MoQDock::SaveSettings);
	connect(qualityToggle, &QCheckBox::toggled, this, &MoQDock::OnQualityToggled);
	connect(profileCombo, QOverload<int>::of(&QComboBox::currentIndexChanged), this, [this](int) {
		// Profile owns the recommended path / codec / encoder / audio. Path and
		// codec edits only rebuild the dependent lists without wiping those picks.
		RefreshQualityOptions(true);
		SaveSettings();
	});
	connect(pathCombo, QOverload<int>::of(&QComboBox::currentIndexChanged), this, [this](int) {
		RefreshQualityOptions();
		SaveSettings();
	});
	connect(videoCodecCombo, QOverload<int>::of(&QComboBox::currentIndexChanged), this, [this](int) {
		RefreshQualityOptions();
		SaveSettings();
	});
	connect(videoEncoderCombo, QOverload<int>::of(&QComboBox::currentIndexChanged), this, &MoQDock::SaveSettings);
	connect(audioCodecCombo, QOverload<int>::of(&QComboBox::currentIndexChanged), this, &MoQDock::SaveSettings);

	RefreshQualityOptions(true);
	LoadSettings();
	ApplyView();
	SetRunning(false);

	{
		std::lock_guard<std::mutex> lock(stopCookie->dockMutex);
		stopCookie->dock = this;
	}
}

MoQDock::~MoQDock()
{
	// Mark closing first so a deferred OBS stop callback refuses begin(), then
	// disconnect and clear the dock pointer before waiting for in-flight work.
	stopCookie->bridge->markClosing();
	StopStream();
	{
		std::lock_guard<std::mutex> lock(stopCookie->dockMutex);
		stopCookie->dock = nullptr;
	}
	stopCookie->bridge->waitIdle(std::chrono::seconds(2));
}

void MoQDock::ToggleStream()
{
	if (running) {
		StopStream();
	} else {
		StartStream();
	}
}

void MoQDock::OpenAdvanced()
{
	MoQAdvancedDialog dialog(advanced, this);
	if (dialog.exec() == QDialog::Accepted)
		SaveSettings();
}

void MoQDock::OnRelayUrlEdited()
{
	PeelJwtFromRelayUrl();
	SaveSettings();
}

void MoQDock::PeelJwtFromRelayUrl()
{
	QString text = urlEdit->text().trimmed();
	QUrl url(text);
	if (!url.isValid() || url.scheme().isEmpty())
		return;

	QUrlQuery query(url);
	if (!query.hasQueryItem("jwt"))
		return;

	const QString jwt = query.queryItemValue("jwt", QUrl::FullyDecoded).trimmed();
	if (jwt.isEmpty())
		return;

	tokenEdit->setText(jwt);
	query.removeAllQueryItems("jwt");
	url.setQuery(query);
	QSignalBlocker block(urlEdit);
	urlEdit->setText(url.toString());
}

static bool IsCleartextDialScheme(const QString &scheme)
{
	return scheme.compare(QStringLiteral("ws"), Qt::CaseInsensitive) == 0 ||
	       scheme.compare(QStringLiteral("http"), Qt::CaseInsensitive) == 0;
}

QString MoQDock::ConnectUrl() const
{
	QString text = urlEdit->text().trimmed();
	const QString token = tokenEdit->text().trimmed();
	if (token.isEmpty())
		return text;

	QUrl url(text);
	if (!url.isValid() || url.scheme().isEmpty()) {
		// Fall back to string append when Qt can't parse (unusual schemes).
		const QChar sep = text.contains('?') ? '&' : '?';
		return text + sep + "jwt=" + QUrl::toPercentEncoding(token);
	}

	// Never put a JWT on cleartext dial URLs (ws:// / http://).
	if (IsCleartextDialScheme(url.scheme()))
		return text;

	QUrlQuery query(url);
	query.removeAllQueryItems("jwt");
	query.addQueryItem("jwt", token);
	url.setQuery(query);
	return url.toString();
}

void MoQDock::OnQualityToggled(bool enabled)
{
	qualityBox->setEnabled(enabled);
	if (enabled)
		RefreshQualityOptions();
	SaveSettings();
}

void MoQDock::RefreshQualityOptions(bool applyProfileDefaults)
{
	const auto offers = EnumerateEncoders();

	std::vector<MoQEncoderCapability> caps;
	caps.reserve(offers.size());
	QStringList hwNames;
	for (const auto &o : offers) {
		caps.push_back({o.codec.toStdString(), o.hardware, o.video});
		if (o.video && o.hardware && !hwNames.contains(o.display))
			hwNames.append(o.display);
	}

	const QString profile = profileCombo->currentData().toString();
	const MoQQualityDefaults recommended = RecommendQualityDefaults(profile.toStdString(), caps);
	const bool haveHw = recommended.path == "hardware";
	const bool high = recommended.high;

	// Only reset path when the profile itself changed. Auto must not wipe a
	// manual Software/Hardware pick on every dependent refresh.
	if (applyProfileDefaults) {
		QSignalBlocker bPath(pathCombo);
		SelectComboData(pathCombo, QString::fromStdString(recommended.path));
	}

	const QString savedCodec = videoCodecCombo->currentData().toString();
	const QString savedEncoder = videoEncoderCombo->currentData().toString();
	const QString savedAudio = audioCodecCombo->currentData().toString();
	const QString pathPref = pathCombo->currentData().toString();
	const bool preferHw = pathPref != "software";

	QSignalBlocker bCodec(videoCodecCombo);
	QSignalBlocker bEnc(videoEncoderCombo);
	QSignalBlocker bAud(audioCodecCombo);

	videoCodecCombo->clear();
	videoEncoderCombo->clear();
	audioCodecCombo->clear();

	bool haveH264 = false, haveHevc = false, haveAv1 = false;
	for (const auto &o : offers) {
		if (!o.video)
			continue;
		if (o.codec == "h264")
			haveH264 = true;
		else if (o.codec == "hevc")
			haveHevc = true;
		else if (o.codec == "av1")
			haveAv1 = true;
	}

	if (haveH264)
		videoCodecCombo->addItem("H.264", "h264");
	if (haveHevc)
		videoCodecCombo->addItem("HEVC", "hevc");
	if (haveAv1)
		videoCodecCombo->addItem("AV1", "av1");
	if (videoCodecCombo->count() == 0)
		videoCodecCombo->addItem("H.264 (fallback)", "h264");

	if (applyProfileDefaults)
		SelectComboData(videoCodecCombo, QString::fromStdString(recommended.video_codec));
	else if (!savedCodec.isEmpty())
		SelectComboData(videoCodecCombo, savedCodec);
	else
		videoCodecCombo->setCurrentIndex(0);

	const QString codec = videoCodecCombo->currentData().toString();
	videoEncoderCombo->addItem("Auto", "auto");
	for (const auto &o : offers) {
		if (!o.video || o.codec != codec)
			continue;
		if (preferHw && !o.hardware)
			continue;
		if (!preferHw && o.hardware)
			continue;
		videoEncoderCombo->addItem(o.display, o.id);
	}
	// If the preference filter left only Auto, list every matching codec encoder.
	if (videoEncoderCombo->count() == 1) {
		for (const auto &o : offers) {
			if (!o.video || o.codec != codec)
				continue;
			videoEncoderCombo->addItem(o.display, o.id);
		}
	}

	if (applyProfileDefaults)
		SelectComboData(videoEncoderCombo, QString::fromStdString(recommended.video_encoder));
	else if (!savedEncoder.isEmpty())
		SelectComboData(videoEncoderCombo, savedEncoder);
	else
		videoEncoderCombo->setCurrentIndex(0);

	bool haveAac = false, haveOpus = false;
	for (const auto &o : offers) {
		if (o.video)
			continue;
		if (o.codec == "aac")
			haveAac = true;
		else if (o.codec == "opus")
			haveOpus = true;
	}
	if (haveAac)
		audioCodecCombo->addItem("AAC", "aac");
	if (haveOpus)
		audioCodecCombo->addItem("Opus", "opus");
	if (audioCodecCombo->count() == 0)
		audioCodecCombo->addItem("AAC (fallback)", "aac");

	if (applyProfileDefaults)
		SelectComboData(audioCodecCombo, QString::fromStdString(recommended.audio_codec));
	else if (!savedAudio.isEmpty())
		SelectComboData(audioCodecCombo, savedAudio);
	else
		audioCodecCombo->setCurrentIndex(0);

	QString detected = haveHw ? QString("Detected: hardware available (%1)").arg(hwNames.join(", "))
				  : QString("Detected: software encoders only");
	if (profile == "auto")
		detected += preferHw ? " · Auto → Quality" : " · Auto → Performance";
	else if (high)
		detected += QStringLiteral(" · Quality · 8 Mbps");
	else
		detected += QStringLiteral(" · Performance · 2.5 Mbps");
	detectedLabel->setText(detected);
}

bool MoQDock::CreateConfiguredEncoders()
{
	if (qualityToggle->isChecked())
		return CreateTranscodeEncoders();

	config_t *config = obs_frontend_get_profile_config();
	if (!config) {
		LOG_ERROR("No profile config available");
		return false;
	}

	const char *mode = config_get_string(config, "Output", "Mode");
	const bool advancedMode = mode && strcmp(mode, "Advanced") == 0;

	OBSDataAutoRelease videoSettings = obs_data_create();
	OBSDataAutoRelease audioSettings = obs_data_create();
	const char *videoId = nullptr;
	const char *audioId = nullptr;
	int audioBitrate = 0;
	size_t audioMixerIdx = 0;

	if (advancedMode) {
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

	// MoQ publishes inline headers (avc3/hev1), so force repeat_headers
	obs_data_set_bool(videoSettings, "repeat_headers", true);
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
	const QString videoCodec = NormalizeCodec(obs_get_encoder_codec(videoId));
	const QString audioCodec = NormalizeCodec(obs_get_encoder_codec(audioId));
	const char *videoDisplay = obs_encoder_get_display_name(videoId);
	const char *audioDisplay = obs_encoder_get_display_name(audioId);
	publishSummary = QString("Preset OBS Output\nVideo %1 · %2\nAudio %3 · %4 · %5 kbps")
				 .arg(videoCodec.isEmpty() ? QStringLiteral("video") : videoCodec.toUpper(),
				      videoDisplay && *videoDisplay ? QString::fromUtf8(videoDisplay)
								    : QString::fromUtf8(videoId),
				      audioCodec.isEmpty() ? QStringLiteral("audio") : audioCodec.toUpper(),
				      audioDisplay && *audioDisplay ? QString::fromUtf8(audioDisplay)
								    : QString::fromUtf8(audioId))
				 .arg(audioBitrate);
	return true;
}

bool MoQDock::CreateTranscodeEncoders()
{
	const auto offers = EnumerateEncoders();
	const QString profile = profileCombo->currentData().toString();
	const bool preferHw = pathCombo->currentData().toString() != "software";
	const bool high = profile == "high" || (profile == "auto" && preferHw);
	const QString codec = videoCodecCombo->currentData().toString();
	const QString encoderChoice = videoEncoderCombo->currentData().toString();
	const QString audioChoice = audioCodecCombo->currentData().toString();

	QString videoId;
	if (encoderChoice != "auto" && !encoderChoice.isEmpty()) {
		videoId = encoderChoice;
	} else {
		const EncoderOffer *pick = nullptr;
		for (const auto &o : offers) {
			if (!o.video || o.codec != codec)
				continue;
			if (!pick) {
				pick = &o;
				continue;
			}
			if (preferHw && o.hardware && !pick->hardware)
				pick = &o;
			else if (!preferHw && !o.hardware && pick->hardware)
				pick = &o;
		}
		if (pick)
			videoId = pick->id;
	}
	if (videoId.isEmpty())
		videoId = "obs_x264";

	QString audioId = audioChoice == "opus" ? "ffmpeg_opus" : "ffmpeg_aac";
	for (const auto &o : offers) {
		if (!o.video && o.codec == audioChoice) {
			audioId = o.id;
			break;
		}
	}

	const int videoBitrate = high ? 8000 : 2500;
	const int audioBitrate = high ? 192 : 96;

	OBSDataAutoRelease videoSettings = obs_data_create();
	OBSDataAutoRelease audioSettings = obs_data_create();
	obs_data_set_int(videoSettings, "bitrate", videoBitrate);
	obs_data_set_string(videoSettings, "rate_control", "CBR");
	obs_data_set_bool(videoSettings, "repeat_headers", true);
	obs_data_set_int(audioSettings, "bitrate", audioBitrate);

	videoEncoder = OBSEncoderAutoRelease(
		obs_video_encoder_create(videoId.toUtf8().constData(), "moq_dock_video", videoSettings, nullptr));
	size_t audioMixerIdx = 0;
	if (config_t *config = obs_frontend_get_profile_config()) {
		const char *mode = config_get_string(config, "Output", "Mode");
		if (mode && strcmp(mode, "Advanced") == 0) {
			int track = (int)config_get_int(config, "AdvOut", "TrackIndex");
			if (track < 1 || track > 6)
				track = 1;
			audioMixerIdx = (size_t)(track - 1);
		}
	}

	audioEncoder = OBSEncoderAutoRelease(obs_audio_encoder_create(audioId.toUtf8().constData(), "moq_dock_audio",
								      audioSettings, audioMixerIdx, nullptr));
	if (!videoEncoder || !audioEncoder) {
		LOG_ERROR("Failed to create transcode encoders (%s / %s)", videoId.toUtf8().constData(),
			  audioId.toUtf8().constData());
		return false;
	}

	obs_encoder_set_video(videoEncoder, obs_get_video());
	obs_encoder_set_audio(audioEncoder, obs_get_audio());

	LOG_INFO("Using dock transcode encoders: %s / %s (profile %s, %d kbps)", videoId.toUtf8().constData(),
		 audioId.toUtf8().constData(), high ? "quality" : "performance", videoBitrate);

	QString profileLabel = high ? QStringLiteral("Quality") : QStringLiteral("Performance");
	if (profile == "auto")
		profileLabel = high ? QStringLiteral("Auto → Quality") : QStringLiteral("Auto → Performance");
	const char *videoDisplay = obs_encoder_get_display_name(videoId.toUtf8().constData());
	const char *audioDisplay = obs_encoder_get_display_name(audioId.toUtf8().constData());
	const QString pathLabel = preferHw ? QStringLiteral("Hardware") : QStringLiteral("Software");
	publishSummary = QString("Preset %1 · %2\nVideo %3 · %4 · %5 kbps\nAudio %6 · %7 · %8 kbps")
				 .arg(profileLabel, pathLabel,
				      codec.isEmpty() ? QStringLiteral("video") : codec.toUpper(),
				      videoDisplay && *videoDisplay ? QString::fromUtf8(videoDisplay) : videoId)
				 .arg(videoBitrate)
				 .arg(audioChoice.isEmpty() ? QStringLiteral("audio") : audioChoice.toUpper(),
				      audioDisplay && *audioDisplay ? QString::fromUtf8(audioDisplay) : audioId)
				 .arg(audioBitrate);
	return true;
}

void MoQDock::StartStream()
{
	PeelJwtFromRelayUrl();

	const QString relayText = urlEdit->text().trimmed();
	if (relayText.isEmpty()) {
		status->setText("Relay URL is required");
		return;
	}

	const QString token = tokenEdit->text().trimmed();
	if (!token.isEmpty()) {
		const QUrl dial(relayText);
		if (dial.isValid() && IsCleartextDialScheme(dial.scheme())) {
			status->setText("Publish token requires https:// or wss:// (not cleartext ws:// or http://)");
			return;
		}
	}

	const std::string url = ConnectUrl().toStdString();
	const std::string path = pathEdit->text().toStdString();

	SaveSettings();

	// The MoQ output reads the server URL / path from its attached service, so
	// build a throwaway service from the dock fields.
	OBSDataAutoRelease serviceSettings = obs_data_create();
	// The advanced settings ride along on the service, which is where the output reads
	// them from regardless of whether the dock or Settings -> Stream configured it.
	obs_data_apply(serviceSettings, advanced);
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

	signal_handler_connect(obs_output_get_signal_handler(output), "stop", OnOutputStopped, stopCookie.get());

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
		signal_handler_disconnect(obs_output_get_signal_handler(output), "stop", OnOutputStopped,
					  stopCookie.get());
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
	tokenEdit->setEnabled(!isRunning);
	pathEdit->setEnabled(!isRunning);
	// The settings are read once at connect, so editing them mid-stream would look
	// like it applied when it hadn't.
	advancedButton->setEnabled(!isRunning);
	qualityToggle->setEnabled(!isRunning);
	qualityBox->setEnabled(!isRunning && qualityToggle->isChecked());

	if (!isRunning) {
		status->setText("● Disconnected");
		status->setStyleSheet("color: #888888;");
		ClearLiveStats();
	}
}

void MoQDock::ApplyView()
{
	statsBox->setVisible(showStats->isChecked());
	sparkBox->setVisible(showTimeline->isChecked());
}

void MoQDock::ClearLiveStats()
{
	statsBox->setText("Waiting for the first connect.");
	rttSpark->Clear();
	sendSpark->Clear();
	recvSpark->Clear();
	lossSpark->Clear();
	sentSpark->Clear();
	liveClock.invalidate();
	publishSummary.clear();
}

void MoQDock::UpdateStatus()
{
	if (!output || !running)
		return;

	auto *moq = static_cast<MoQOutput *>(obs_obj_get_data(output));
	const int reconnects = moq ? moq->GetReconnectCount() : 0;

	int failCode = 0;
	std::string failReason;
	if (moq)
		moq->CopyLastFailure(&failCode, &failReason);
	const QString failText = ExplainFailure(failCode, failReason);

	const bool everConnected = moq && (moq->IsLiveSession() || obs_output_get_connect_time_ms(output) > 0);
	if (!everConnected) {
		if (!failText.isEmpty()) {
			status->setText(QString("● %1").arg(failText));
			status->setStyleSheet("color: #c0392b;");
			if (showStats->isChecked())
				statsBox->setText(failText);
		} else {
			status->setText("● Connecting…");
			status->setStyleSheet("color: #d08b1d;");
			if (showStats->isChecked() && !publishSummary.isEmpty())
				statsBox->setText(publishSummary + QStringLiteral("\nConnecting…"));
		}
		return;
	}

	auto pushGap = [this]() {
		if (!showTimeline->isChecked())
			return;
		rttSpark->Push(false, 0);
		sendSpark->Push(false, 0);
		recvSpark->Push(false, 0);
		lossSpark->Push(false, 0);
		sentSpark->Push(false, 0);
	};

	MoQOutput::ConnectionStats stats;
	if (!moq || !moq->TryGetConnectionStats(&stats)) {
		// Refresh failure text after the stats call; it may have just recorded one.
		if (moq)
			moq->CopyLastFailure(&failCode, &failReason);
		const QString latest = ExplainFailure(failCode, failReason);

		if (!latest.isEmpty() &&
		    (latest.contains(QStringLiteral("Unauthorized")) || latest.contains(QStringLiteral("Forbidden")))) {
			QString text = QString("● %1").arg(latest);
			if (reconnects > 0)
				text += QString(" · reconnects %1").arg(reconnects);
			status->setText(text);
			status->setStyleSheet("color: #c0392b;");
			if (showStats->isChecked())
				statsBox->setText(latest);
			pushGap();
			return;
		}

		QString text = "● Reconnecting…";
		if (reconnects > 0)
			text += QString(" · reconnects %1").arg(reconnects);
		if (!latest.isEmpty())
			text += QString(" · ") + latest;
		status->setText(text);
		status->setStyleSheet("color: #d08b1d;");
		if (showStats->isChecked()) {
			QStringList lines;
			if (!publishSummary.isEmpty())
				lines << publishSummary;
			lines << (latest.isEmpty() ? QString("Offline · waiting for reconnect.") : latest);
			statsBox->setText(lines.join('\n'));
		}
		pushGap();
		return;
	}

	status->setText(stats.reconnects > 0 ? QString("● Connected · reconnects %1").arg(stats.reconnects)
					     : QString("● Connected"));
	status->setStyleSheet("color: #36a45e;");

	if (showStats->isChecked()) {
		QStringList lines;
		if (!publishSummary.isEmpty())
			lines << publishSummary;
		if (!stats.protocol.empty())
			lines << QString("Protocol %1").arg(QString::fromStdString(stats.protocol));
		if (!stats.dial.empty())
			lines << QString("Dial %1").arg(QString::fromStdString(stats.dial));
		statsBox->setText(lines.isEmpty() ? QString("Connected.") : lines.join('\n'));
	}

	if (showTimeline->isChecked()) {
		if (!liveClock.isValid())
			liveClock.start();
		rttSpark->Push(stats.rtt_valid, stats.rtt_ms);
		sendSpark->Push(stats.send_rate_valid, stats.send_rate_bps);
		recvSpark->Push(stats.recv_rate_valid, stats.recv_rate_bps);
		lossSpark->Push(stats.loss_valid, stats.loss_pct);
		sentSpark->Push(stats.bytes_sent_valid, static_cast<double>(stats.bytes_sent));
	}
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
	const char *token = obs_data_get_string(data, "token");
	const char *broadcast = obs_data_get_string(data, "path");
	if (url && *url)
		urlEdit->setText(url);
	if (obs_data_has_user_value(data, "token"))
		tokenEdit->setText(token ? token : "");
	if (obs_data_has_user_value(data, "path"))
		pathEdit->setText(broadcast ? broadcast : "");

	// Older dock.json may still store ?jwt= on the URL; peel it into the token field.
	PeelJwtFromRelayUrl();

	// Applied over the defaults set in the constructor, so a settings file written by
	// an older build (missing keys that have since been added) still loads.
	OBSDataAutoRelease saved = obs_data_get_obj(data, "advanced");
	if (saved)
		obs_data_apply(advanced, saved);

	{
		QSignalBlocker bStats(showStats);
		QSignalBlocker bTimeline(showTimeline);
		if (obs_data_has_user_value(data, "show_stats"))
			showStats->setChecked(obs_data_get_bool(data, "show_stats"));
		if (obs_data_has_user_value(data, "show_timeline"))
			showTimeline->setChecked(obs_data_get_bool(data, "show_timeline"));
	}

	obs_data_t *qualityRaw = obs_data_get_obj(data, "quality");
	if (!qualityRaw)
		qualityRaw = obs_data_get_obj(data, "transcode");
	if (qualityRaw) {
		OBSDataAutoRelease quality(qualityRaw);
		QSignalBlocker bToggle(qualityToggle);
		QSignalBlocker bProfile(profileCombo);
		QSignalBlocker bPath(pathCombo);
		QSignalBlocker bCodec(videoCodecCombo);
		QSignalBlocker bEnc(videoEncoderCombo);
		QSignalBlocker bAud(audioCodecCombo);
		const bool enabled = obs_data_get_bool(quality, "enabled");
		qualityToggle->setChecked(enabled);
		qualityBox->setEnabled(enabled);

		SelectComboData(profileCombo, QString::fromUtf8(obs_data_get_string(quality, "profile")));
		SelectComboData(pathCombo, QString::fromUtf8(obs_data_get_string(quality, "path")));
		RefreshQualityOptions();
		SelectComboData(videoCodecCombo, QString::fromUtf8(obs_data_get_string(quality, "video_codec")));
		RefreshQualityOptions();
		SelectComboData(videoEncoderCombo, QString::fromUtf8(obs_data_get_string(quality, "video_encoder")));
		SelectComboData(audioCodecCombo, QString::fromUtf8(obs_data_get_string(quality, "audio_codec")));
	}
}

void MoQDock::SaveSettings()
{
	const std::string path = SettingsPath();
	if (path.empty())
		return;

	QDir().mkpath(QFileInfo(QString::fromStdString(path)).absolutePath());

	OBSDataAutoRelease data = obs_data_create();
	obs_data_set_string(data, "url", urlEdit->text().toUtf8().constData());
	obs_data_set_string(data, "token", tokenEdit->text().toUtf8().constData());
	obs_data_set_string(data, "path", pathEdit->text().toUtf8().constData());
	obs_data_set_bool(data, "show_stats", showStats->isChecked());
	obs_data_set_bool(data, "show_timeline", showTimeline->isChecked());
	obs_data_set_obj(data, "advanced", advanced);

	OBSDataAutoRelease quality = obs_data_create();
	obs_data_set_bool(quality, "enabled", qualityToggle->isChecked());
	obs_data_set_string(quality, "profile", profileCombo->currentData().toString().toUtf8().constData());
	obs_data_set_string(quality, "path", pathCombo->currentData().toString().toUtf8().constData());
	obs_data_set_string(quality, "video_codec", videoCodecCombo->currentData().toString().toUtf8().constData());
	obs_data_set_string(quality, "video_encoder", videoEncoderCombo->currentData().toString().toUtf8().constData());
	obs_data_set_string(quality, "audio_codec", audioCodecCombo->currentData().toString().toUtf8().constData());
	obs_data_set_obj(data, "quality", quality);

	obs_data_save_json(data, path.c_str());
}

void MoQDock::OnOutputStopped(void *data, calldata_t *params)
{
	auto *cookie = static_cast<StopCookie *>(data);
	if (!cookie || !cookie->bridge || !cookie->bridge->begin())
		return;

	// Covers only the OBS-thread read of the dock pointer and failure text.
	// The queued Qt work uses QPointer, so it must not hold the activity count
	// or destruction would wait on the event loop.
	struct EndBridge {
		std::shared_ptr<MoQDockStopBridge> bridge;
		~EndBridge()
		{
			if (bridge)
				bridge->end();
		}
	} endBridge{cookie->bridge};

	MoQDock *self = nullptr;
	{
		std::lock_guard<std::mutex> lock(cookie->dockMutex);
		self = cookie->dock;
	}
	if (!self)
		return;

	const long long code = calldata_int(params, "code");
	auto *stopped = static_cast<obs_output_t *>(calldata_ptr(params, "output"));

	// Capture failure text from the stopped output on this thread. Do not read
	// self->output here: a concurrent StartStream may already have replaced it.
	int failCode = 0;
	std::string failReason;
	auto *moq = stopped ? static_cast<MoQOutput *>(obs_obj_get_data(stopped)) : nullptr;
	if (moq)
		moq->CopyLastFailure(&failCode, &failReason);
	if (failReason.empty()) {
		const char *err = calldata_string(params, "last_error");
		if ((!err || !*err) && stopped)
			err = obs_output_get_last_error(stopped);
		if (err && *err)
			failReason = err;
	}
	const QString failText = ExplainFailure(failCode, failReason);

	// Queue onto the application object, not the dock: the dock may be destroyed
	// before the event runs. QPointer makes a late delivery a no-op.
	const QPointer<MoQDock> dock(self);
	QMetaObject::invokeMethod(
		qApp,
		[dock, code, failText, stopped]() {
			if (!dock)
				return;
			// A late stop for a superseded output must not tear down a newer Go Live.
			if (stopped && dock->output && static_cast<obs_output_t *>(dock->output) != stopped)
				return;
			dock->StopStream();
			if (code == OBS_OUTPUT_SUCCESS)
				return;
			if (!failText.isEmpty()) {
				dock->status->setText(QString("● %1").arg(failText));
				dock->status->setStyleSheet("color: #c0392b;");
				if (dock->showStats->isChecked())
					dock->statsBox->setText(failText);
			} else {
				dock->status->setText(QString("● Stopped (code %1)").arg(code));
				dock->status->setStyleSheet("color: #c0392b;");
			}
		},
		Qt::QueuedConnection);
}

void register_moq_dock()
{
	// OBS takes ownership of the widget; create it without a parent.
	auto *dock = new MoQDock();
	obs_frontend_add_dock_by_id("moq_dock", "MoQ", dock);
}
