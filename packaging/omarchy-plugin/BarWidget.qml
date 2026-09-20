import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Qt.labs.folderlistmodel
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
  property int remainingSeconds: 0
  property var pendingTransfer: null
  property var peerSeen: ({})
  property int peerTtlMs: 30000
  property bool browseOpen: false
  property string browseFolder: ""
  property var browseStack: []
  property var browsePicked: []

  function openBrowse() {
    root.browseFolder = String(root.receivePath || "").trim() || "/home/cristi"
    root.browseStack = []
    root.browsePicked = []
    root.browseOpen = true
  }
  function closeBrowse() {
    root.browseOpen = false
    root.browseStack = []
    root.browsePicked = []
  }
  function upFolder() {
    if (root.browseStack.length > 0) root.browseFolder = root.browseStack.pop()
  }
  function enterFolder(name) {
    if (!name || name === "." || name === "..") return
    root.browseStack.push(root.browseFolder)
    root.browseFolder = root.browseFolder.replace(/\/+$/, "") + "/" + name
  }
  function togglePick(name) {
    var pickedPath = root.browseFolder.replace(/\/+$/, "") + "/" + name
    var idx = root.browsePicked.indexOf(pickedPath)
    if (idx >= 0) root.browsePicked.splice(idx, 1)
    else root.browsePicked.push(pickedPath)
  }
  function pickCount() { return root.browsePicked.length }
  function usePicked() {
    if (root.browsePicked.length === 0) return
    var picked = root.browsePicked.slice()
    root.browseOpen = false
    root.browsePicked = []
    root.browseStack = []
    if (root.selectedFiles.trim().length > 0) {
      var existing = root.selectedFiles.trim().split("\n")
      for (var i = 0; i < existing.length; i++) if (picked.indexOf(existing[i]) < 0) picked.push(existing[i])
    }
    root.selectedFiles = picked.join("\n")
  }

  readonly property bool busy: statusProc.running || actionProc.running
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

  function persistSettings(values) {
    var entry = { id: root.moduleName }
    for (var existing in root.settings) if (existing !== "id") entry[existing] = root.settings[existing]
    for (var key in values) entry[key] = values[key]
    root.settings = entry
    if (root.hostWidget && "settings" in root.hostWidget) root.hostWidget.settings = entry
    if (root.bar && root.bar.shell && typeof root.bar.shell.updateEntryInline === "function")
      root.bar.shell.updateEntryInline(root.moduleName, entry)
  }

  function byteLength(value) { return unescape(encodeURIComponent(value)).length }

  function validateSettings() {
    var name = String(root.transferName || "").trim()
    if (name.length === 0) return "Device name can't be empty"
    if (root.byteLength(name) > 63) return "Device name must be at most 63 bytes"
    var dir = String(root.receivePath || "").trim()
    if (dir.length === 0) return "Receive folder can't be empty"
    if (dir.charAt(0) !== "/") return "Receive folder must be an absolute path"
    return ""
  }

  function applyStatus(raw) {
    try {
      var value = JSON.parse(String(raw).trim())
      root.phase = value.state || "offline"
      root.message = value.message || root.phase
      root.pendingTransfer = value.data || null
      if (root.phase === "receiving") {
        if (!root.receiveActive) {
          root.receiveActive = true
          root.remainingSeconds = 600
        }
      } else if (root.receiveActive) {
        root.receiveActive = false
        root.remainingSeconds = 0
      }
      if (root.phase !== "error") root.errorText = ""
    } catch (e) {
      root.phase = "offline"
      root.message = "Service unavailable"
    }
  }

  function applyPeers(raw) {
    try {
      var value = JSON.parse(String(raw).trim())
      var prevAddress = root.selectedPeer >= 0 && root.peers.length > root.selectedPeer
        ? String(root.peers[root.selectedPeer].address || "") : ""
      var fresh = value.data || []
      var now = Date.now()
      var seenInFresh = {}
      for (var f = 0; f < fresh.length; f++) {
        var fk = String(fresh[f].address || "")
        if (fk.length > 0) seenInFresh[fk] = true
      }
      var merged = []
      var mergedSeen = {}
      for (var o = 0; o < root.peers.length; o++) {
        var peer = root.peers[o]
        var ok = String(peer.address || "")
        if (seenInFresh[ok]) continue
        if (root.peerSeen[ok] && now - root.peerSeen[ok] < root.peerTtlMs) {
          mergedSeen[ok] = root.peerSeen[ok]
          merged.push(peer)
        }
      }
      for (var n = 0; n < fresh.length; n++) {
        var nk = String(fresh[n].address || "")
        if (nk.length === 0) continue
        mergedSeen[nk] = now
        merged.push(fresh[n])
      }
      root.peers = merged
      root.peerSeen = mergedSeen
      root.selectedPeer = -1
      for (var i = 0; i < merged.length; i++) {
        if (String(merged[i].address || "") === prevAddress) { root.selectedPeer = i; break }
      }
      if (root.selectedPeer < 0 && root.peers.length > 0) root.selectedPeer = 0
      root.message = value.message || (root.peers.length + " recipient(s)")
      root.errorText = value.ok === false ? root.message : ""
    } catch (e) {
      root.peers = []
      root.peerSeen = {}
      root.selectedPeer = -1
      root.errorText = "Could not read recipients"
    }
  }

  function startAction(op) {
    if (actionProc.running) return
    var command = ["/usr/bin/ac-dc", "ctl", op]
    if (op === "receive") {
      command.push("--name"); command.push(root.transferName.trim() || "Omarchy")
      command.push("--directory"); command.push(root.receivePath.trim() || "/home/cristi/Downloads/Adhoc")
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
    var command = ["/usr/bin/ac-dc", "ctl", "send", "--host", root.peerHost(peer), "--port", String(root.peerPort(peer)), "--name", root.transferName.trim() || "Omarchy"]
    var files = root.selectedFiles.split("\n").filter(function(path) { return path.trim().length > 0 })
    files.forEach(function(path) { command.push("--file"); command.push(path.trim()) })
    if (root.linkText.trim().length > 0) { command.push("--link"); command.push(root.linkText.trim()) }
    root.errorText = ""
    actionProc.command = command
    actionProc.running = true
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
    onExited: root.refresh()
  }
  Process {
    id: legacyProc
    command: ["/usr/bin/ac-dc", "ui"]
    onExited: function(code) { if (code !== 0) root.errorText = "The full window failed to open" }
  }

  FolderListModel {
    id: folderModel
    folder: root.browseOpen ? ("file://" + root.browseFolder) : "file:///"
    showDirs: true
    showFiles: true
    showDirsFirst: true
    showDotAndDotDot: false
    sortField: FolderListModel.Name
    sortCaseSensitive: false
  }

  Timer { interval: root.popupOpen ? 1500 : 4000; running: true; repeat: true; triggeredOnStart: true; onTriggered: root.refresh() }
  Timer {
    interval: root.peers.length > 0 ? 1000 : 5000
    running: root.popupOpen
    repeat: true
    onTriggered: { if (!root.receiveActive) root.findRecipients() }
  }
  Timer {
    interval: 1000
    running: root.receiveActive
    repeat: true
    onTriggered: { if (root.remainingSeconds > 0) root.remainingSeconds-- }
  }
  Component.onCompleted: root.refresh()
  onPopupOpenChanged: {
    if (root.popupOpen) {
      root.refresh()
      if (!root.receiveActive) root.findRecipients()
    }
  }

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
    onPressed: {
      if (root.phase === "error" || root.errorText.length > 0) root.refresh()
      root.togglePopup()
    }
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
          Text { text: root.phase === "sending" ? "Transfer in progress…" : root.message; color: root.errorText.length > 0 ? Color.urgent : Qt.rgba(1,1,1,0.62); elide: Text.ElideRight; Layout.fillWidth: true; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption }
        }
      }

      PanelSeparator { Layout.fillWidth: true }

      RowLayout {
        Layout.fillWidth: true
        Text { text: "Receive"; color: Color.foreground; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption; font.bold: true }
        Button {
          Layout.fillWidth: true
          text: root.receiveActive ? ("Stop receive · " + root.formatRemaining()) : "Receive 10 min"
          selected: root.receiveActive
          enabled: !root.busy
          onClicked: root.startAction(root.receiveActive ? "stop" : "receive")
        }
      }

      RowLayout {
        Layout.fillWidth: true
        Text { text: "Send to"; color: Color.foreground; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption; font.bold: true }
        Button { text: root.configOpen ? "Save" : "Settings"; onClicked: {
          if (root.configOpen) {
            var error = root.validateSettings()
            if (error.length > 0) { root.errorText = error; return }
            root.persistSettings({ name: root.transferName.trim() || "Omarchy", directory: root.receivePath.trim() || "/home/cristi/Downloads/Adhoc" })
          }
          root.configOpen = !root.configOpen
        } }
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
        Text {
          text: "Incoming transfer from " + (root.pendingTransfer ? root.pendingTransfer.sender : "Nearby device")
          color: Color.foreground
          font.family: root.font(Style.font.body)
          font.pixelSize: Style.font.body
          font.bold: true
        }
        Text {
          text: root.itemsLabel()
          color: Qt.rgba(1,1,1,0.65)
          elide: Text.ElideRight
          Layout.fillWidth: true
          font.family: root.font(Style.font.caption)
          font.pixelSize: Style.font.caption
        }
        RowLayout {
          Layout.fillWidth: true
          Button {
            text: "Reject"
            enabled: !root.busy
            onClicked: root.decideTransfer("reject")
          }
          Button {
            text: "Approve"
            enabled: !root.busy
            onClicked: root.decideTransfer("approve")
          }
        }
      }
      ColumnLayout {
        visible: root.peers.length > 0; Layout.fillWidth: true; spacing: Style.space(5)
        Repeater {
          model: root.peers
          delegate: Button {
            required property var modelData; required property int index
            text: "≋  " + (modelData.name || modelData.instance || "Nearby device")
            foreground: root.selectedPeer === index ? Color.accent : Color.foreground
            Layout.fillWidth: true
            onClicked: root.selectedPeer = index
          }
        }
      }
      DropArea {
        visible: !root.browseOpen
        Layout.fillWidth: true
        Layout.preferredHeight: Style.space(72)
        onDropped: function(drop) {
          var paths = []
          for (var i = 0; i < drop.urls.length; i++) {
            var value = String(drop.urls[i])
            if (value.indexOf("file://") === 0) value = decodeURIComponent(value.slice(7))
            paths.push(value)
          }
          if (paths.length > 0) root.selectedFiles = paths.join("\n")
          drop.acceptProposedAction()
        }
        Rectangle {
          anchors.fill: parent
          radius: Style.cornerRadius
          color: parent.containsDrag ? Qt.rgba(0.25, 0.55, 0.95, 0.22) : Qt.rgba(1, 1, 1, 0.06)
          border.color: parent.containsDrag ? Color.accent : Qt.rgba(1, 1, 1, 0.16)
          border.width: 1
          ColumnLayout {
            anchors.centerIn: parent
            Text { Layout.alignment: Qt.AlignHCenter; text: "≋"; color: Color.accent; font.pixelSize: Style.font.title }
            Text { Layout.alignment: Qt.AlignHCenter; text: root.selectedFiles.length ? (root.selectedFiles.split("\n").length + " file(s) ready") : "Drop files here to send"; color: Color.foreground; font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption }
            Button { Layout.alignment: Qt.AlignHCenter; text: "Browse…"; enabled: !root.busy; onClicked: root.openBrowse() }
          }
        }
      }
      ColumnLayout {
        visible: root.browseOpen
        Layout.fillWidth: true
        spacing: Style.space(5)
        RowLayout {
          Layout.fillWidth: true
          Button { text: "↩"; enabled: root.browseStack.length > 0; onClicked: root.upFolder() }
          Text {
            Layout.fillWidth: true
            text: root.browseFolder
            color: Qt.rgba(1, 1, 1, 0.72)
            elide: Text.ElideMiddle
            font.family: root.font(Style.font.caption)
            font.pixelSize: Style.font.caption
          }
          Button { text: "Cancel"; onClicked: root.closeBrowse() }
        }
        ListView {
          Layout.fillWidth: true
          Layout.preferredHeight: Style.space(220)
          clip: true
          model: folderModel
          delegate: Item {
            required property int index
            width: parent ? parent.width : 0
            height: Style.space(28)
            Button {
              anchors.fill: parent
              text: {
                var name = folderModel.get(index, "fileName") || ""
                if (folderModel.isFolder(index)) return name + "  /"
                return name
              }
              foreground: folderModel.isFolder(index) ? Color.accent : Color.foreground
              onClicked: {
                if (folderModel.isFolder(index)) root.enterFolder(folderModel.get(index, "fileName"))
                else root.togglePick(folderModel.get(index, "fileName"))
              }
            }
          }
          ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }
        }
        RowLayout {
          Layout.fillWidth: true
          Text { text: root.pickCount() + " file(s) picked"; color: Qt.rgba(1, 1, 1, 0.6); font.family: root.font(Style.font.caption); font.pixelSize: Style.font.caption }
          Button { text: "Add"; enabled: root.pickCount() > 0; Layout.alignment: Qt.AlignRight; onClicked: root.usePicked() }
        }
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

  function itemsLabel() {
    var items = root.pendingTransfer ? (root.pendingTransfer.items || []) : []
    if (items.length > 3) return items.length + " items"
    return items.join(", ")
  }
}
