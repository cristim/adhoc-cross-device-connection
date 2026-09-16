import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

BarWidget {
  id: root
  moduleName: "org.omarchy.adhoccrossdeviceconnection"
  property bool popupOpen: false
  property string phase: "offline"
  property string message: "Airdrop-compatible service unavailable"

  function refresh() { if (!statusProc.running) statusProc.running = true }
  function applyStatus(raw) {
    try {
      var v = JSON.parse(String(raw).trim())
      root.phase = v.state || "offline"
      root.message = v.message || root.phase
    } catch (e) {
      root.phase = "offline"
      root.message = "Airdrop-compatible service unavailable"
    }
  }
  function action(op) {
    if (actionProc.running) return
    actionProc.command = ["/usr/bin/ac-dc", "ctl", op]
    actionProc.running = true
  }
  function openLegacyUi() {
    if (actionProc.running) return
    actionProc.command = ["/usr/bin/ac-dc", "ui"]
    actionProc.running = true
  }

  Process {
    id: statusProc
    command: ["/usr/bin/ac-dc", "ctl", "status"]
    stdout: StdioCollector { waitForEnd: true; onStreamFinished: root.applyStatus(text) }
  }
  Process {
    id: actionProc
    onExited: { root.refresh(); root.popupOpen = true }
  }
  Timer { interval: 3000; running: true; repeat: true; triggeredOnStart: true; onTriggered: root.refresh() }
  Component.onCompleted: root.refresh()

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight
  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.phase === "receiving" ? "󰄀" : "󰇚"
    slotSize: Style.bar.statusSlot
    fontSize: Style.font.caption
    foreground: root.phase === "error" ? Color.urgent : (root.phase === "offline" ? Qt.rgba(1,1,1,0.45) : (root.bar ? root.bar.barForeground : Color.foreground))
    tooltipText: "Airdrop-compatible: " + root.message
    onPressed: root.popupOpen = !root.popupOpen
  }
  PopupCard {
    id: popup
    anchorItem: button
    bar: root.bar
    owner: root
    open: root.popupOpen
    contentWidth: fittedContentWidth(Style.space(280))
    contentHeight: fittedContentHeight(content.implicitHeight)
    ColumnLayout {
      id: content
      width: parent.width
      spacing: Style.space(10)
      Text { text: "ADHOC CONNECTION"; color: Color.foreground; font.family: root.bar ? root.bar.fontFamily : Style.font.family; font.pixelSize: Style.font.caption; font.bold: true }
      Text { text: root.message; color: Qt.rgba(1,1,1,0.7); wrapMode: Text.WordWrap; Layout.fillWidth: true; font.family: root.bar ? root.bar.fontFamily : Style.font.family; font.pixelSize: Style.font.caption }
      RowLayout {
        Layout.fillWidth: true; spacing: Style.space(6)
        Button { text: root.phase === "receiving" ? "Stop" : "Receive"; onClicked: root.action(root.phase === "receiving" ? "stop" : "receive") }
        Button { text: "Send"; onClicked: root.openLegacyUi() }
      }
    }
  }
}
