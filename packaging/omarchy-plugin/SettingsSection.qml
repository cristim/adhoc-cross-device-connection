import QtQuick 6.11
import QtQuick.Controls
import QtQuick.Layouts
import qs.Commons
import qs.Ui

ColumnLayout {
  id: settings
  Layout.fillWidth: true
  spacing: Style.space(8)

  required property string deviceName
  required property string receiveDir
  required property bool bleWake
  required property string validationError
  required property var fontFn

  signal nameEdited(string value)
  signal dirEdited(string value)
  signal bleWakeToggled(bool value)
  signal saveRequested()

  TextField {
    Layout.fillWidth: true
    placeholderText: "Device name (e.g. Omarchy)"
    text: settings.deviceName
    onTextChanged: settings.nameEdited(text)
  }

  TextField {
    Layout.fillWidth: true
    placeholderText: "Save directory (absolute path)"
    text: settings.receiveDir
    onTextChanged: settings.dirEdited(text)
  }

  RowLayout {
    Layout.fillWidth: true
    Text {
      Layout.fillWidth: true
      text: "Bluetooth wake advertisement"
      textFormat: Text.PlainText
      color: Color.foreground
      font.family: settings.fontFn(Style.font.caption)
      font.pixelSize: Style.font.caption
    }
    Button {
      text: settings.bleWake ? "On" : "Off"
      selected: settings.bleWake
      onClicked: settings.bleWakeToggled(!settings.bleWake)
    }
  }

  Text {
    visible: settings.validationError.length > 0
    text: settings.validationError
    textFormat: Text.PlainText
    color: Color.urgent
    wrapMode: Text.WordWrap
    Layout.fillWidth: true
    font.family: settings.fontFn(Style.font.caption)
    font.pixelSize: Style.font.caption
  }

  Button {
    Layout.fillWidth: true
    text: "Save settings"
    onClicked: settings.saveRequested()
  }
}
