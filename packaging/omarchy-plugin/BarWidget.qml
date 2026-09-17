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
  property bool configOpen: false
  property string phase: "offline"
  property string message: "Service unavailable"
  property var peers: []
  property int selectedPeer: -1
  property string selectedFiles: ""
  property string linkText: ""
  property string errorText: ""
  property string transferName: String(setting("name", "Omarchy"))
  property string receivePath: String(setting("directory", "/home/cristi/Downloads/Adhoc"))
  property bool receiveActive: false
  property double receiveEndsAt: 0
  property int remainingSeconds: 0
  property var pendingTransfer: null

  readonly property bool busy: statusProc.running || peersProc.running || actionProc.running || fileProc.running
  readonly property bool canSend: selectedPeer >= 0 && (selectedFiles.trim().length > 0 || linkText.trim().length > 0)

  function font(size) { return root.bar ? root.bar.fontFamily : Style.font.family }
  function close() { root.popupOpen = false }
  function togglePopup() { root.popupOpen = !root.popupOpen }
  function refresh() { if (!statusProc.running) statusProc.running = true }
  function formatRemaining() {
    var minutes = Math.floor(root.remainingSeconds / 60)
    var seconds = root.remainingSeconds % 60
    return minutes + ":" + (seconds < 10 ? "0" : "") + seconds
  }
  function startReceiveCountdown() {
    root.receiveActive = true
    root.receiveEndsAt = Date.now() + 600000
    root.remainingSeconds = 600
  }
  function stopReceiveCountdown() {
    root.receiveActive = false
    root.receiveEndsAt = 0
    root.remainingSeconds = 0
  }

  function persistSettings(values) {
    var entry = { id: root.moduleName }
    for (var existing in root.settings) if (existing !== "id") entry[existing] = root.settings[existing]
    for (var key in values) entry[key] = values[key]
    root.settings = entry
    if (root.hostWidget && "settings" in root.hostWidget) root.hostWidget.settings = entry
    if (root.bar && root.bar.shell && typeof root.bar.shell.updateEntryInline === "function")
      root.bar.shell.updateEntryInline(root.moduleName, entry)
  }

  function applyStatus(raw) {
    try {
      var value = JSON.parse(String(raw).trim())
      root.phase = value.state || "offline"
      root.message = value.message || root.phase
      root.pendingTransfer = value.data || null
      if (root.phase === "receiving" && !root.receiveActive) root.startReceiveCountdown()
      if ((root.phase === "idle" || root.phase === "error" || root.phase === "offline") && root.receiveActive && !actionProc.running)
        root.stopReceiveCountdown()
      if (root.phase !== "error") root.errorText = ""
    } catch (e) {
      root.phase = "offline"
      root.message = "Service unavailable"
    }
  }

  function applyPeers(raw) {
    try {
      var value = JSON.parse(String(raw).trim())
      root.peers = value.data || []
      root.selectedPeer = root.peers.length ? 0 : -1
      root.message = value.message || (root.peers.length + " recipient(s)")
      root.errorText = value.ok === false ? root.message : ""
    } catch (e) {
      root.peers = []
      root.selectedPeer = -1
      root.errorText = "Could not read recipients"
    }
  }

  function startAction(op) {
    if (actionProc.running) return
    if (op === "receive") root.startReceiveCountdown()
    if (op === "stop") root.stopReceiveCountdown()
    var command = ["/usr/bin/ac-dc", "ctl", op]
    if (op === "receive") {
      command.push("--name"); command.push(root.transferName || "Omarchy")
      command.push("--directory"); command.push(root.receivePath || "/home/cristi/Downloads/Adhoc")
    }
    actionProc.command = command
    actionProc.running = true
  }
  function decideTransfer(op) { if (!actionProc.running) { actionProc.command = ["/usr/bin/ac-dc", "ctl", op]; actionProc.running = true } }

  function findRecipients() {
    if (peersProc.running) return
    root.errorText = ""
    peersProc.running = true
  }

  function peerHost(peer) {
    var address = String(peer.address || "")
    var match = address.match(/^\[(.*)\]:(\d+)$/)
    return match ? match[1] : address
  }

  function peerPort(peer) {
    var address = String(peer.address || "")
    var match = address.match(/^\[(.*)\]:(\d+)$/)
    return match ? Number(match[2]) : 8770
  }

  function send() {
    if (!root.canSend || actionProc.running) return
    var peer = root.peers[root.selectedPeer]
    var command = ["/usr/bin/ac-dc", "ctl", "send", "--host", root.peerHost(peer), "--port", String(root.peerPort(peer)), "--name", root.transferName || "Omarchy"]
    var files = root.selectedFiles.split("\n").filter(function(path) { return path.trim().length > 0 })
    files.forEach(function(path) { command.push("--file"); command.push(path.trim()) })
    if (root.linkText.trim().length > 0) { command.push("--link"); command.push(root.linkText.trim()) }
    root.errorText = ""
    actionProc.command = command
    actionProc.running = true
  }

  function chooseFiles() { if (!fileProc.running) fileProc.running = true }
  function applyFiles(raw) {
    var value = String(raw).trim()
    if (value.length > 0) root.selectedFiles = value.replace(/\r/g, "")
  }

  Process {
    id: statusProc
    command: ["/usr/bin/ac-dc", "ctl", "status"]
    stdout: StdioCollector { waitForEnd: true; onStreamFinished: root.applyStatus(text) }
    onExited: function(code) { if (code !== 0) root.phase = "offline" }
  }
  Process {
    id: peersProc
    command: ["/usr/bin/ac-dc", "ctl", "peers"]
    stdout: StdioCollector { waitForEnd: true; onStreamFinished: root.applyPeers(text) }
    onExited: function(code) { if (code !== 0) root.errorText = "Recipient discovery failed" }
  }
  Process {
    id: actionProc
    stdout: StdioCollector { waitForEnd: true; onStreamFinished: {
      var raw = String(text).trim()
      if (raw.length > 0) {
        try {
          var value = JSON.parse(raw)
          if (value.ok === false) root.errorText = value.message || "Action failed"
          root.message = value.message || root.message
        } catch (e) { root.errorText = "Action failed" }
      }
    } }
    onExited: { root.refresh(); root.popupOpen = true }
  }
  Process {
    id: fileProc
    command: ["/usr/bin/zenity", "--file-selection", "--multiple", "--separator=\n", "--title=Choose files to send"]
    stdout: StdioCollector { waitForEnd: true; onStreamFinished: root.applyFiles(text) }
  }
  Process { id: legacyProc; command: ["/usr/bin/ac-dc", "ui"] }

  Timer { interval: root.popupOpen ? 1500 : 4000; running: true; repeat: true; triggeredOnStart: true; onTriggered: root.refresh() }
  Timer { interval: 1000; running: root.popupOpen; repeat: true; triggeredOnStart: true; onTriggered: root.findRecipients() }
  Timer {
    interval: 1000
    running: root.receiveActive
    repeat: true
    triggeredOnStart: true
    onTriggered: {
      root.remainingSeconds = Math.max(0, Math.ceil((root.receiveEndsAt - Date.now()) / 1000))
      if (root.remainingSeconds === 0) root.stopReceiveCountdown()
    }
  }
  Component.onCompleted: root.refresh()

  visible: true
  implicitWidth: Style.bar.statusSlot
  implicitHeight: Style.bar.statusSlot
  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.phase === "receiving" ? "󰄀" : (root.phase === "sending" ? "󰇚" : "≋")
    slotSize: Style.bar.statusSlot
    fontSize: Style.font.caption
    foreground: root.phase === "error" || root.errorText.length > 0 ? Color.urgent : (root.phase === "offline" ? Qt.rgba(1,1,1,0.45) : (root.bar ? root.bar.barForeground : Color.foreground))
    tooltipText: "Adhoc: " + root.message
    onPressed: root.togglePopup()
  }

  PopupCard {
    id: popup
    anchorItem: root
    bar: root.bar
    owner: root
    open: root.popupOpen
    contentWidth: popup.fittedContentWidth(Style.space(410))
    contentHeight: popup.fittedContentHeight(content.implicitHeight)

    ColumnLayout {
      id: content
      width: parent.width
      spacing: Style.space(10)

      RowLayout {
        Layout.fillWidth: true
        spacing: Style.space(9)
        BorderSurface {
          Layout.preferredWidth: Style.space(38); Layout.preferredHeight: Style.space(38)
          radius: Style.spacing.labelGap
          color: Style.normalFillFor(root.bar ? root.bar.foreground : Color.foreground, Color.accent)
          Text { anchors.centerIn: parent; text: "≋"; color: Color.accent; font.family: root.font(Style.font.subtitle); font.pixelSize: Style.font.subtitle }
        }
        ColumnLayout {
          Layout.fillWidth: true; spacing: Style.space(2)
          Text { text: "Adhoc Connection"; color: Color.foreground; font.family: root.font(Style.font.body); font.pixelSize: Style.font.body; font.bold: true }
          Text { text: root.message; color: root.errorText.length > 0 ? Color.urgent : Qt.rgba(1,1,1,0.62); elide: Text.ElideRight; Layout.fillWidth: true; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption }
        }
        Button { text: root.receiveActive ? ("Stop " + root.formatRemaining()) : "Receive"; enabled: !root.busy || root.receiveActive; onClicked: root.startAction(root.receiveActive ? "stop" : "receive") }
      }

      PanelSeparator { Layout.fillWidth: true }

      RowLayout {
        Layout.fillWidth: true
        Text { text: "Send to"; color: Color.foreground; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption; font.bold: true }
        Button { text: root.configOpen ? "Save" : "Settings"; onClicked: { if (root.configOpen) root.persistSettings({ name: root.transferName || "Omarchy", directory: root.receivePath || "/home/cristi/Downloads/Adhoc" }); root.configOpen = !root.configOpen } }
      }

      ColumnLayout {
        visible: root.configOpen
        Layout.fillWidth: true; spacing: Style.space(5)
        TextField { Layout.fillWidth: true; placeholderText: "Device name"; text: root.transferName; onTextChanged: root.transferName = text }
        TextField { Layout.fillWidth: true; placeholderText: "Receive folder"; text: root.receivePath; onTextChanged: root.receivePath = text }
        Text { text: "Defaults: Omarchy • ~/Downloads/Adhoc"; color: Qt.rgba(1,1,1,0.45); font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption }
      }

      Text { visible: root.peers.length === 0; text: "Find a nearby Apple device to choose a recipient."; color: Qt.rgba(1,1,1,0.55); wrapMode: Text.WordWrap; Layout.fillWidth: true; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption }
      ColumnLayout {
        visible: root.pendingTransfer !== null
        Layout.fillWidth: true; spacing: Style.space(5)
        Text { text: "Incoming transfer from " + (root.pendingTransfer ? root.pendingTransfer.sender : "Nearby device"); color: Color.foreground; font.family: root.font(Style.font.body); font.pixelSize: Style.font.body; font.bold: true }
        Text { text: root.pendingTransfer ? root.pendingTransfer.items.join(", ") : ""; color: Qt.rgba(1,1,1,0.65); elide: Text.ElideRight; Layout.fillWidth: true; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption }
        RowLayout { Layout.fillWidth: true; Button { text: "Reject"; onClicked: root.decideTransfer("reject") }; Button { text: "Approve"; onClicked: root.decideTransfer("approve") } }
      }
      RowLayout {
        visible: root.peers.length > 0; Layout.fillWidth: true; spacing: Style.space(5)
        Repeater {
          model: root.peers
          delegate: Button {
            required property var modelData; required property int index
            text: modelData.name || modelData.instance || "Nearby device"
            foreground: root.selectedPeer === index ? Color.accent : Color.foreground
            Layout.fillWidth: true
            onClicked: root.selectedPeer = index
          }
        }
      }
      RowLayout {
        Layout.fillWidth: true
        Button { text: "Choose files"; enabled: !root.busy; onClicked: root.chooseFiles() }
        Text { text: root.selectedFiles.length ? (root.selectedFiles.split("\n").length + " file(s) selected") : "No files selected"; color: Qt.rgba(1,1,1,0.58); elide: Text.ElideRight; Layout.fillWidth: true; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption }
      }
      TextField { Layout.fillWidth: true; placeholderText: "Or paste an https:// link"; text: root.linkText; onTextChanged: root.linkText = text }
      RowLayout {
        Layout.fillWidth: true
        Button { text: "Send"; enabled: root.canSend && !root.busy; Layout.fillWidth: true; onClicked: root.send() }
        Button { text: "Full window"; enabled: !root.busy; onClicked: { root.popupOpen = false; legacyProc.running = true } }
      }
      Text { visible: root.errorText.length > 0; text: root.errorText; color: Color.urgent; wrapMode: Text.WordWrap; Layout.fillWidth: true; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption }
    }
  }
}
