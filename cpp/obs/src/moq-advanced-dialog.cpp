// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-advanced-dialog.h"
#include "moq-settings.h"

#include <QCheckBox>
#include <QComboBox>
#include <QDialogButtonBox>
#include <QFileDialog>
#include <QFormLayout>
#include <QHBoxLayout>
#include <QLabel>
#include <QLineEdit>
#include <QPushButton>
#include <QScrollArea>
#include <QSpinBox>
#include <QVBoxLayout>

namespace {

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

QWidget *MoQHintLabel(const QString &title, const QString &help, QWidget *parent)
{
	auto *wrap = new QWidget(parent);
	auto *name = new QLabel(title, wrap);
	name->setWordWrap(true);
	auto *hint = new QLabel(QStringLiteral("?"), wrap);
	hint->setFixedSize(16, 16);
	hint->setAlignment(Qt::AlignCenter);
	hint->setCursor(Qt::WhatsThisCursor);
	hint->setStyleSheet("QLabel { color: #9ecbff; background: #2a2a2a; border: 1px solid #555; "
			    "border-radius: 8px; font-size: 10px; font-weight: bold; }");
	if (!help.isEmpty()) {
		name->setToolTip(help);
		hint->setToolTip(help);
		wrap->setToolTip(help);
	} else {
		hint->hide();
	}

	auto *row = new QHBoxLayout(wrap);
	row->setContentsMargins(0, 0, 0, 0);
	row->setSpacing(6);
	row->addWidget(name, 1);
	row->addWidget(hint, 0, Qt::AlignTop);
	return wrap;
}

MoQAdvancedPanel::MoQAdvancedPanel(QWidget *parent) : QWidget(parent)
{
	enabled = new QCheckBox("Use advanced settings", this);
	enabled->setToolTip("With this off, MoQ connects with its defaults and everything below is ignored.");

	form = new QWidget(this);
	auto *layout = new QFormLayout(form);
	layout->setRowWrapPolicy(QFormLayout::WrapAllRows);
	layout->setFieldGrowthPolicy(QFormLayout::AllNonFixedFieldsGrow);

	for (const MoQSettings::Field &field : MoQSettings::Fields()) {
		QWidget *widget = nullptr;
		QWidget *row = nullptr;

		switch (field.kind) {
		case MoQSettings::Kind::Bool: {
			auto *check = new QCheckBox(form);
			widget = row = check;
			connect(check, &QCheckBox::toggled, this, &MoQAdvancedPanel::Notify);
			break;
		}
		case MoQSettings::Kind::Int: {
			auto *spin = new QSpinBox(form);
			spin->setRange((int)field.min, (int)field.max);
			spin->setSingleStep((int)field.step);
			spin->setKeyboardTracking(true);
			widget = row = spin;
			connect(spin, QOverload<int>::of(&QSpinBox::valueChanged), this, &MoQAdvancedPanel::Notify);
			break;
		}
		case MoQSettings::Kind::Text: {
			auto *edit = new QLineEdit(form);
			widget = row = edit;
			connect(edit, &QLineEdit::editingFinished, this, &MoQAdvancedPanel::Notify);
			break;
		}
		case MoQSettings::Kind::File:
		case MoQSettings::Kind::Directory: {
			QLineEdit *edit = nullptr;
			row = PathRow(&edit, field, form);
			widget = edit;
			connect(edit, &QLineEdit::editingFinished, this, &MoQAdvancedPanel::Notify);
			connect(edit, &QLineEdit::textChanged, this, &MoQAdvancedPanel::Notify);
			break;
		}
		case MoQSettings::Kind::Choice: {
			auto *combo = new QComboBox(form);
			combo->setEditable(field.editable);
			for (const MoQSettings::Option &option : field.options)
				combo->addItem(QString::fromUtf8(option.label), QString::fromUtf8(option.value));
			widget = row = combo;
			connect(combo, QOverload<int>::of(&QComboBox::currentIndexChanged), this,
				&MoQAdvancedPanel::Notify);
			if (field.editable)
				connect(combo, &QComboBox::editTextChanged, this, &MoQAdvancedPanel::Notify);
			break;
		}
		}

		const QString help = field.tooltip ? QString::fromUtf8(field.tooltip) : QString();
		if (!help.isEmpty()) {
			widget->setToolTip(help);
			row->setToolTip(help);
		}

		widgets[field.key] = widget;
		layout->addRow(MoQHintLabel(QString::fromUtf8(field.label), help, form), row);
	}

	auto *scroll = new QScrollArea(this);
	scroll->setWidget(form);
	scroll->setWidgetResizable(true);
	scroll->setFrameShape(QFrame::NoFrame);

	connect(enabled, &QCheckBox::toggled, form, &QWidget::setEnabled);
	connect(enabled, &QCheckBox::toggled, this, &MoQAdvancedPanel::Notify);

	auto *enableRow = new QWidget(this);
	auto *enableLayout = new QHBoxLayout(enableRow);
	enableLayout->setContentsMargins(0, 0, 0, 0);
	enableLayout->addWidget(enabled, 1);
	auto *enableHint = new QLabel(QStringLiteral("?"), enableRow);
	enableHint->setFixedSize(16, 16);
	enableHint->setAlignment(Qt::AlignCenter);
	enableHint->setCursor(Qt::WhatsThisCursor);
	enableHint->setToolTip(enabled->toolTip());
	enableHint->setStyleSheet("QLabel { color: #9ecbff; background: #2a2a2a; border: 1px solid #555; "
				  "border-radius: 8px; font-size: 10px; font-weight: bold; }");
	enableLayout->addWidget(enableHint, 0, Qt::AlignTop);

	auto *outer = new QVBoxLayout(this);
	outer->setContentsMargins(0, 0, 0, 0);
	outer->addWidget(enableRow);
	outer->addWidget(scroll, 1);
}

void MoQAdvancedPanel::Notify()
{
	if (!loading)
		emit changed();
}

void MoQAdvancedPanel::Load(obs_data_t *settings)
{
	loading = true;
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
				combo->setEditText(value);
			break;
		}
		}
	}
	loading = false;
}

void MoQAdvancedPanel::Save(obs_data_t *settings)
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
			const int index = combo->findText(combo->currentText());
			const QString value = index >= 0 ? combo->itemData(index).toString() : combo->currentText();
			obs_data_set_string(settings, field.key, value.toUtf8().constData());
			break;
		}
		}
	}
}

MoQAdvancedDialog::MoQAdvancedDialog(obs_data_t *settings, QWidget *parent)
	: QDialog(parent),
	  settings(settings),
	  panel(new MoQAdvancedPanel(this))
{
	setWindowTitle("MoQ Advanced Settings");
	setModal(true);

	panel->Load(settings);

	auto *buttons = new QDialogButtonBox(QDialogButtonBox::Ok | QDialogButtonBox::Cancel, this);
	connect(buttons, &QDialogButtonBox::accepted, this, [this]() {
		panel->Save(this->settings);
		accept();
	});
	connect(buttons, &QDialogButtonBox::rejected, this, &QDialog::reject);

	auto *outer = new QVBoxLayout(this);
	outer->addWidget(panel, 1);
	outer->addWidget(buttons);

	resize(520, 600);
}
