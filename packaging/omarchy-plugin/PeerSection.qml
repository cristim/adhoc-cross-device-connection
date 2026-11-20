import QtQuick 6.11
import QtQuick.Controls
import QtQuick.Layouts
import qs.Commons
import qs.Ui

ColumnLayout {
  id: section
  Layout.fillWidth: true
  spacing: Style.space(8)

  required property var peers
  required property int selectedPeer
  required property var selectedFiles
  required property string linkText
  required property bool busy
  required property bool sending
  required property var activeTransfer
  required property var fontFn

  signal refreshPeers()
  signal selectPeer(int index)
  signal filesDropped(var urls)
  signal clearFiles()
  signal linkEdited(string text)
  signal sendRequested()
  signal cancelRequested()
  signal openFullUi()

  RowLayout {
    Layout.fillWidth: true
    spacing: Style.space(6)
    Text {
      text: "Send"
      textFormat: Text.PlainText
      color: Color.foreground
      font.family: section.fontFn(Style.font.caption)
      font.pixelSize: Style.font.caption
      font.bold: true
      Layout.fillWidth: true
    }
    Button {
      text: "Find peers"
      enabled: !section.busy
      onClicked: section.refreshPeers()
    }
    Button {
      text: "Files…"
      onClicked: section.openFullUi()
    }
  }

  ColumnLayout {
    Layout.fillWidth: true
    spacing: Style.space(4)
    visible: section.peers.length > 0

    Repeater {
      model: section.peers
      delegate: Button {
        required property int index
        required property var modelData
        Layout.fillWidth: true
        selected: index === section.selectedPeer
        text: String(modelData.name || "Unknown").slice(0, 64)
        onClicked: section.selectPeer(index)
      }
    }
  }

  Text {
    visible: section.peers.length === 0
    text: "No nearby peers yet. Click Find peers to scan."
    textFormat: Text.PlainText
    color: Qt.rgba(1, 1, 1, 0.55)
    wrapMode: Text.WordWrap
    Layout.fillWidth: true
    font.family: section.fontFn(Style.font.caption)
    font.pixelSize: Style.font.caption
  }

  BorderSurface {
    Layout.fillWidth: true
    Layout.preferredHeight: Style.space(44)
    radius: Style.spacing.labelGap
    color: dropArea.containsDrag
      ? Style.normalFillFor(Color.accent, Color.accent)
      : Qt.rgba(1, 1, 1, 0.04)

    DropArea {
      id: dropArea
      anchors.fill: parent
      onDropped: function(drop) {
        if (drop.hasUrls) section.filesDropped(drop.urls)
      }
    }

    RowLayout {
      anchors.fill: parent
      anchors.margins: Style.space(8)
      spacing: Style.space(6)
      Text {
        Layout.fillWidth: true
        textFormat: Text.PlainText
        elide: Text.ElideRight
        color: section.selectedFiles.length > 0 ? Color.foreground : Qt.rgba(1, 1, 1, 0.5)
        text: section.selectedFiles.length > 0
          ? (section.selectedFiles.length + " file(s) ready")
          : "Drop files here to send"
        font.family: section.fontFn(Style.font.caption)
        font.pixelSize: Style.font.caption
      }
      Button {
        visible: section.selectedFiles.length > 0
        text: "Clear"
        onClicked: section.clearFiles()
      }
    }
  }

  TextField {
    Layout.fillWidth: true
    placeholderText: "Optional http(s):// link to share"
    text: section.linkText
    onTextChanged: section.linkEdited(text)
  }

  RowLayout {
    Layout.fillWidth: true
    spacing: Style.space(6)
    visible: section.sending && section.activeTransfer !== null

    Text {
      Layout.fillWidth: true
      textFormat: Text.PlainText
      color: Color.foreground
      elide: Text.ElideRight
      text: {
        var t = section.activeTransfer
        if (!t) return ""
        var pct = (t.total_bytes && t.total_bytes > 0)
          ? (" · " + Math.min(100, Math.floor((t.sent_bytes * 100) / t.total_bytes)) + "%")
          : ""
        return "Sending to " + String(t.target || "") + pct
      }
      font.family: section.fontFn(Style.font.caption)
      font.pixelSize: Style.font.caption
    }
    Button {
      text: "Cancel"
      onClicked: section.cancelRequested()
    }
  }

  // send.rs rejects a request unless exactly one of files/links is set
  // (files.is_empty() != links.is_empty()), so the button must match.
  function hasExactlyOneTarget() {
    return (section.selectedFiles.length > 0) !== (section.linkText.trim().length > 0)
  }

  Text {
    visible: section.selectedFiles.length > 0 && section.linkText.trim().length > 0
    text: "Choose files or a link, not both"
    textFormat: Text.PlainText
    color: Color.urgent
    wrapMode: Text.WordWrap
    Layout.fillWidth: true
    font.family: section.fontFn(Style.font.caption)
    font.pixelSize: Style.font.caption
  }

  Button {
    Layout.fillWidth: true
    visible: !section.sending
    text: "Send"
    enabled: !section.busy
      && section.selectedPeer >= 0
      && section.hasExactlyOneTarget()
    onClicked: section.sendRequested()
  }
}
