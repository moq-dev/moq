#pragma once

#include <QWidget>
#include <obs.hpp>

#include <chrono>

class QLineEdit;
class QPushButton;
class QLabel;
class QTimer;

// A dockable panel that drives the MoQ output directly, without relying on the
// core Settings -> Stream UI (which does not surface third-party services on
// stable OBS yet). The dock owns its own service/output/encoder objects and
// reuses the encoder settings configured in OBS's Output settings.
class MoQDock : public QWidget {
	Q_OBJECT

public:
	explicit MoQDock(QWidget *parent = nullptr);
	~MoQDock() override;

private slots:
	void ToggleStream();
	void UpdateStats();

private:
	void StartStream();
	void StopStream();
	void SetRunning(bool running);
	bool CreateConfiguredEncoders();

	void LoadSettings();
	void SaveSettings();

	// Output "stop" signal handler. Fires on a non-UI thread, so it marshals
	// back to the Qt thread before touching widgets.
	static void OnOutputStopped(void *data, calldata_t *params);

	QLineEdit *urlEdit;
	QLineEdit *pathEdit;
	QPushButton *button;
	QLabel *status;

	QLabel *statState;
	QLabel *statDuration;
	QLabel *statBitrate;
	QLabel *statSent;
	QLabel *statDropped;
	QLabel *statConnect;

	QTimer *statsTimer;

	OBSServiceAutoRelease service;
	OBSOutputAutoRelease output;
	OBSEncoderAutoRelease videoEncoder;
	OBSEncoderAutoRelease audioEncoder;

	bool running = false;
	uint64_t lastBytes = 0;
	std::chrono::steady_clock::time_point lastSample;
	std::chrono::steady_clock::time_point streamStart;
};

void register_moq_dock();
