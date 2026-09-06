// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <QPointer>
#include <QWidget>
#include <obs.hpp>

#include <memory>

#include "moq-dock-stop.h"

class QLineEdit;
class QPushButton;
class QLabel;
class QComboBox;
class QGroupBox;
class QTimer;
class QTabWidget;
class MoQSpark;

// A dockable panel that drives the MoQ output directly, without relying on the
// core Settings -> Stream UI (which does not surface third-party services on
// stable OBS yet). The dock owns its own service/output/encoder objects and
// reuses the encoder settings configured in OBS's Output settings, unless the
// Encoding tab selects custom settings for this MoQ stream.
class MoQDock : public QWidget {
	Q_OBJECT

public:
	explicit MoQDock(QWidget *parent = nullptr);
	~MoQDock() override;

private slots:
	void ToggleStream();
	void UpdateStatus();
	void OpenAdvanced();
	void OnEncodingChanged(int mode);
	void RefreshEncodingOptions(bool applyProfileDefaults = false);
	void OnRelayUrlEdited();

private:
	void StartStream();
	void StopStream();
	void SetRunning(bool running);
	bool CreateConfiguredEncoders();
	bool CreateCustomEncoders();
	QString ConnectUrl() const;
	void PeelJwtFromRelayUrl();
	void ClearLiveStats();

	void LoadSettings();
	void SaveSettings();

	// Output "stop" signal handler. Fires on a non-UI thread, so it marshals
	// back to the Qt thread before touching widgets.
	static void OnOutputStopped(void *data, calldata_t *params);

	QTabWidget *tabs;
	QLineEdit *urlEdit;
	QLineEdit *tokenEdit;
	QLineEdit *pathEdit;
	QPushButton *button;
	QPushButton *advancedButton;
	QLabel *status;

	QLabel *statsBox;
	QWidget *sparkBox;
	MoQSpark *rttSpark;
	MoQSpark *sendSpark;
	MoQSpark *recvSpark;
	MoQSpark *lossSpark;
	MoQSpark *sentSpark;

	QComboBox *encodingMode;
	QGroupBox *encodingBox;
	QComboBox *profileCombo;
	QLabel *detectedLabel;
	QComboBox *pathCombo;
	QComboBox *videoCodecCombo;
	QComboBox *videoEncoderCombo;
	QComboBox *audioCodecCombo;

	// Advanced connection settings, edited in their own window so the dock stays
	// small. Persisted alongside the URL and path, and copied into the throwaway
	// service at StartStream so the output reads them the same way it does for a
	// service configured through Settings -> Stream.
	OBSDataAutoRelease advanced;

	QTimer *pollTimer;

	OBSServiceAutoRelease service;
	OBSOutputAutoRelease output;
	OBSEncoderAutoRelease videoEncoder;
	OBSEncoderAutoRelease audioEncoder;

	bool running = false;
	// Filled when Go Live creates encoders; shown in Stats while publishing.
	QString publishSummary;

	// Shared with OnOutputStopped so a deferred OBS stop callback cannot race
	// destruction. The signal user_data is this cookie, not the dock pointer.
	struct StopCookie {
		std::shared_ptr<MoQDockStopBridge> bridge = std::make_shared<MoQDockStopBridge>();
		std::mutex dockMutex;
		MoQDock *dock = nullptr;
	};
	std::shared_ptr<StopCookie> stopCookie = std::make_shared<StopCookie>();
};

void register_moq_dock();
