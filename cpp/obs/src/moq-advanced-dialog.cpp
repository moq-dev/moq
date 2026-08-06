// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-advanced-dialog.h"
#include "moq-settings.h"

#include <QCheckBox>
#include <QComboBox>
#include <QDialogButtonBox>
#include <QFileDialog>
#include <QFormLayout>
#include <QHBoxLayout>
#include <QLineEdit>
#include <QPushButton>
#include <QScrollArea>
#include <QSpinBox>
#include <QVBoxLayout>

namespace {

// A path row: an editable field plus a Browse button that opens the matching picker.
// The QLineEdit is what gets registered as the field's widget, so Load/Save stay uniform.
QWidget *PathRow(QLineEdit **out, const MoQSettings::Field &field, QWidget *parent)
{
	auto *row = new QWidget(parent);
	auto *edit = new QLineEdit(row);
	auto *browse = new QPushButton("Browse…", row);

	const bool directory = field.kind == MoQSettings::Kind::Directory;
	const QString label = QString::fromUtf8(field.label);
	const QString filter = field.filter ? QString::fromUtf8(field.filter) : QString();

	QObject::connect(browse, &QPushButton::clicked, edit, [edit, directory, label, filter]() {
		const QString picked = directory ? QFileDialog::getExistingDirectory(edit, label, edit->text())
						 : QFileDialog::getOpenFileName(edit, label, edit->text(), filter);
		if (!picked.isEmpty())
			edit->setText(picked);
	});

	auto *layout = new QHBoxLayout(row);
	layout->setContentsMargins(0, 0, 0, 0);
	layout->addWidget(edit, 1);
	layout->addWidget(browse);

	*out = edit;
	return row;
}

} // namespace

MoQAdvancedDialog::MoQAdvancedDialog(obs_data_t *settings, QWidget *parent)
	: QDialog(parent),
	  settings(settings),
	  form(nullptr),
	  enabled(nullptr)
{
	setWindowTitle("MoQ Advanced Settings");
	setModal(true);

	enabled = new QCheckBox("Use advanced settings", this);
	enabled->setToolTip("With this off, MoQ connects with its defaults and everything below is ignored.");

	form = new QWidget(this);
	auto *layout = new QFormLayout(form);
	layout->setFieldGrowthPolicy(QFormLayout::AllNonFixedFieldsGrow);

	for (const MoQSettings::Field &field : MoQSettings::Fields()) {
		QWidget *widget = nullptr;
		QWidget *row = nullptr;

		switch (field.kind) {
		case MoQSettings::Kind::Bool: {
			auto *check = new QCheckBox(form);
			widget = row = check;
			break;
		}
		case MoQSettings::Kind::Int: {
			auto *spin = new QSpinBox(form);
			spin->setRange((int)field.min, (int)field.max);
			spin->setSingleStep((int)field.step);
			// Typing is faster than stepping through a 600000ms range.
			spin->setKeyboardTracking(true);
			widget = row = spin;
			break;
		}
		case MoQSettings::Kind::Text: {
			auto *edit = new QLineEdit(form);
			widget = row = edit;
			break;
		}
		case MoQSettings::Kind::File:
		case MoQSettings::Kind::Directory: {
			QLineEdit *edit = nullptr;
			row = PathRow(&edit, field, form);
			widget = edit;
			break;
		}
		case MoQSettings::Kind::Choice: {
			auto *combo = new QComboBox(form);
			combo->setEditable(field.editable);
			for (const MoQSettings::Option &option : field.options)
				combo->addItem(QString::fromUtf8(option.label), QString::fromUtf8(option.value));
			widget = row = combo;
			break;
		}
		}

		if (field.tooltip) {
			const QString tooltip = QString::fromUtf8(field.tooltip);
			widget->setToolTip(tooltip);
			row->setToolTip(tooltip);
		}

		widgets[field.key] = widget;
		layout->addRow(QString::fromUtf8(field.label), row);
	}

	// The form is long; scroll it rather than growing a window taller than the screen.
	auto *scroll = new QScrollArea(this);
	scroll->setWidget(form);
	scroll->setWidgetResizable(true);

	auto *buttons = new QDialogButtonBox(QDialogButtonBox::Ok | QDialogButtonBox::Cancel, this);
	connect(buttons, &QDialogButtonBox::accepted, this, [this]() {
		Save();
		accept();
	});
	connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);

	// Grey the form out when the settings are off, so it's obvious nothing below applies.
	connect(enabled, &QCheckBox::toggled, form, &QWidget::setEnabled);

	auto *outer = new QVBoxLayout(this);
	outer->addWidget(enabled);
	outer->addWidget(scroll, 1);
	outer->addWidget(buttons);

	resize(520, 600);

	Load();
}

void MoQAdvancedDialog::Load()
{
	enabled->setChecked(obs_data_get_bool(settings, MoQSettings::ENABLED));
	form->setEnabled(enabled->isChecked());

	for (const MoQSettings::Field &field : MoQSettings::Fields()) {
		QWidget *widget = widgets[field.key];

		switch (field.kind) {
		case MoQSettings::Kind::Bool:
			static_cast<QCheckBox *>(widget)->setChecked(obs_data_get_bool(settings, field.key));
			break;
		case MoQSettings::Kind::Int:
			static_cast<QSpinBox *>(widget)->setValue((int)obs_data_get_int(settings, field.key));
			break;
		case MoQSettings::Kind::Text:
		case MoQSettings::Kind::File:
		case MoQSettings::Kind::Directory:
			static_cast<QLineEdit *>(widget)->setText(
				QString::fromUtf8(obs_data_get_string(settings, field.key)));
			break;
		case MoQSettings::Kind::Choice: {
			auto *combo = static_cast<QComboBox *>(widget);
			const QString value = QString::fromUtf8(obs_data_get_string(settings, field.key));
			const int index = combo->findData(value);
			if (index >= 0)
				combo->setCurrentIndex(index);
			else if (combo->isEditable())
				// A value the library accepts but doesn't advertise, e.g. a
				// work-in-progress draft. Keep it rather than snapping to the default.
				combo->setEditText(value);
			break;
		}
		}
	}
}

void MoQAdvancedDialog::Save()
{
	obs_data_set_bool(settings, MoQSettings::ENABLED, enabled->isChecked());

	for (const MoQSettings::Field &field : MoQSettings::Fields()) {
		QWidget *widget = widgets[field.key];

		switch (field.kind) {
		case MoQSettings::Kind::Bool:
			obs_data_set_bool(settings, field.key, static_cast<QCheckBox *>(widget)->isChecked());
			break;
		case MoQSettings::Kind::Int:
			obs_data_set_int(settings, field.key, static_cast<QSpinBox *>(widget)->value());
			break;
		case MoQSettings::Kind::Text:
		case MoQSettings::Kind::File:
		case MoQSettings::Kind::Directory:
			obs_data_set_string(settings, field.key,
					    static_cast<QLineEdit *>(widget)->text().toUtf8().constData());
			break;
		case MoQSettings::Kind::Choice: {
			auto *combo = static_cast<QComboBox *>(widget);
			// An editable combo's text is only a listed value when the user picked
			// one; a typed value has no item data, so read the text in that case.
			const int index = combo->findText(combo->currentText());
			const QString value = index >= 0 ? combo->itemData(index).toString() : combo->currentText();
			obs_data_set_string(settings, field.key, value.toUtf8().constData());
			break;
		}
		}
	}
}
