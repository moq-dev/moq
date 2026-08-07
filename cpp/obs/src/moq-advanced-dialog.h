// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <obs.hpp>

#include <QDialog>
#include <QWidget>

#include <map>
#include <string>

// A separate window for the advanced MoQ settings, so the dock stays small.
//
// The form is generated from MoQSettings::Fields(), the same list the service properties
// page is built from, so a knob added there shows up in both without touching this file.
class MoQAdvancedDialog : public QDialog {
	Q_OBJECT

public:
	// Edits `settings` in place on accept. The caller keeps ownership and should
	// persist it afterwards.
	explicit MoQAdvancedDialog(obs_data_t *settings, QWidget *parent = nullptr);

private:
	// Fill the widgets from the settings data.
	void Load();
	// Write the widgets back into the settings data.
	void Save();

	obs_data_t *settings;
	QWidget *form;
	class QCheckBox *enabled;

	// Widgets by setting key, so Load/Save can walk Fields() rather than naming each
	// one twice more.
	std::map<std::string, QWidget *> widgets;
};
