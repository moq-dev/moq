// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <obs.hpp>

#include <QDialog>
#include <QString>
#include <QWidget>

#include <map>
#include <string>

// Label plus a ? hint that shows `help` on hover.
QWidget *MoQHintLabel(const QString &title, const QString &help, QWidget *parent);

// The advanced connection form, generated from MoQSettings::Fields(). Shared by
// the dock tab and the Advanced… dialog so both edit the same obs_data.
class MoQAdvancedPanel : public QWidget {
	Q_OBJECT

public:
	explicit MoQAdvancedPanel(QWidget *parent = nullptr);

	void Load(obs_data_t *settings);
	void Save(obs_data_t *settings);

signals:
	void changed();

private:
	void Notify();

	QWidget *form = nullptr;
	class QCheckBox *enabled = nullptr;
	std::map<std::string, QWidget *> widgets;
	bool loading = false;
};

class MoQAdvancedDialog : public QDialog {
	Q_OBJECT

public:
	// Edits `settings` in place on accept. The caller keeps ownership and should
	// persist it afterwards.
	explicit MoQAdvancedDialog(obs_data_t *settings, QWidget *parent = nullptr);

private:
	obs_data_t *settings;
	MoQAdvancedPanel *panel;
};
