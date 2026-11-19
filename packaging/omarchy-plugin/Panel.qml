import QtQuick 6.11
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Event-driven AirDrop-compatible bar panel backed by `ac-dc ctl subscribe`.
// Zero polling timers: the user daemon owns receive windows, radio state, and transfers.
Panel {
  id: root
  moduleName: "org.omarchy.adhoccrossdeviceconnection"
  manageIpc: false

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  readonly property string homeDir: Quickshell.env("HOME") || "/tmp"

  function resolveDir(raw) {
    var s = String(raw === undefined || raw === null ? "" : raw).trim()
    if (s.length === 0) return root.homeDir + "/Downloads/Adhoc"
    if (s === "~") return root.homeDir
    if (s.indexOf("~/") === 0) return root.homeDir + s.substring(1)
    return s
  }

  function parseMinutes() {
    var n = parseInt(setting("defaultWindowMinutes", 10), 10)
    if (isNaN(n)) return 10
    return Math.max(1, Math.min(60, n))
  }

  function parseDiskPercent(raw) {
    var n = parseInt(raw, 10)
    return isNaN(n) ? 30 : n
  }

  property bool popupOpen: false
  property bool settingsOpen: false

  property string phase: "offline"
  property var windowState: null
  property var radioState: ({ ready: false, visible: null, reason: "" })
  property var pendingTransfer: null
  property var activeTransfer: null
  property var peers: []
  property var clipboardState: null
  property string lastError: ""

  property string selectedMode: "timed"   // "timed" | "once" | "always"
  property int selectedMinutes: parseMinutes()
  property int selectedPeer: -1
  property var selectedFiles: []
  property string linkText: ""

  property string transferName: String(setting("name", "Omarchy"))
  property string receivePath: resolveDir(setting("directory", ""))
  property bool bleWake: Boolean(setting("bleWake", true))
  property bool contactsOnly: Boolean(setting("contactsOnly", false))
  property bool copyToClipboard: Boolean(setting("copyToClipboard", true))
  property string maxDiskPercentText: String(parseDiskPercent(setting("maxDiskPercent", 30)))
  property string settingsError: ""

  readonly property bool receiving: phase === "receiving" || phase === "starting"
  readonly property bool sending: phase === "sending"
  readonly property bool busy: actionProc.running || peersProc.running

  function font(size) { return root.bar ? root.bar.fontFamily : Style.font.family }
  function close() { root.popupOpen = false }
  function togglePopup() { root.popupOpen = !root.popupOpen }

  function sanitizeText(raw, maxLen) {
    var s = String(raw === undefined || raw === null ? "" : raw)
      .replace(/[\u0000-\u001F\u007F]/g, " ")
      .trim()
    return s.length > maxLen ? s.slice(0, maxLen) : s
  }

  function formatSeconds(sec) {
    var s = Math.max(0, Number(sec) || 0)
    var m = Math.floor(s / 60)
    var r = s % 60
    return m + ":" + (r < 10 ? "0" : "") + r
  }

  function statusSummary() {
    if (root.lastError.length > 0) return root.lastError
    if (root.phase === "offline") return "Daemon unavailable"
    if (root.phase === "starting") return "Starting AWDL radio…"
    if (root.phase === "receiving") {
      var scope = root.contactsOnly ? "Contacts Only" : "Everyone"
      if (root.radioState.visible === true) return "Visible (" + scope + ")"
      if (root.radioState.visible === false) return "Receiver up (not advertising)"
      return "Receiving (" + scope + ")"
    }
    if (root.phase === "sending") return "Transfer in progress…"
    return "Ready"
  }

  function receiveButtonLabel() {
    if (!root.receiving) {
      if (root.selectedMode === "once") return "Receive once"
      if (root.selectedMode === "always") return "Receive always"
      return "Receive " + root.selectedMinutes + " min"
    }
    if (root.windowState && root.windowState.mode === "once") return "Stop · Once"
    if (root.windowState && root.windowState.mode === "always") return "Stop · Always"
    if (root.windowState && root.windowState.remaining_seconds !== undefined && root.windowState.remaining_seconds !== null) {
      return "Stop · " + root.formatSeconds(root.windowState.remaining_seconds)
    }
    return "Stop receive"
  }

  function applySnapshot(line) {
    if (!line || line.length > 65536) return
    try {
      var snap = JSON.parse(line)
      if (!snap || typeof snap !== "object") return
      root.phase = root.sanitizeText(snap.phase || "idle", 32)
      root.windowState = snap.window || null
      root.radioState = snap.radio || ({ ready: false, visible: null, reason: "" })
      if (snap.contacts_only !== undefined && root.receiving) {
        root.contactsOnly = Boolean(snap.contacts_only)
      }
      if (snap.pending && typeof snap.pending === "object") {
        var items = Array.isArray(snap.pending.items) ? snap.pending.items.slice(0, 16) : []
        root.pendingTransfer = {
          sender: root.sanitizeText(snap.pending.sender || "Unknown", 128),
          items: items.map(function(i) { return root.sanitizeText(i, 128) })
        }
      } else {
        root.pendingTransfer = null
      }
      root.activeTransfer = snap.active_transfer || null
      if (Array.isArray(snap.peers)) {
        var prevAddr = (root.selectedPeer >= 0 && root.peers.length > root.selectedPeer)
          ? String(root.peers[root.selectedPeer].address || "") : ""
        var list = snap.peers.slice(0, 32).map(function(p) {
          return {
            name: root.sanitizeText(p.name || "Unknown", 128),
            instance: root.sanitizeText(p.instance || "", 128),
            address: root.sanitizeText(p.address || "", 128)
          }
        })
        root.peers = list
        root.selectedPeer = -1
        for (var idx = 0; idx < list.length; idx++) {
          if (list[idx].address === prevAddr) { root.selectedPeer = idx; break }
        }
        if (root.selectedPeer < 0 && list.length > 0) root.selectedPeer = 0
      }
      root.clipboardState = snap.clipboard || null
      root.lastError = snap.last_error ? root.sanitizeText(snap.last_error, 200) : ""
    } catch (e) {
      // Ignore malformed line on stream.
    }
  }

  function runCtl(args) {
    if (actionProc.running) return
    actionProc.command = ["/usr/bin/ac-dc", "ctl"].concat(args)
    actionProc.running = true
  }

  function toggleReceive() {
    if (root.receiving) {
      root.runCtl(["stop"])
      return
    }
    var pct = parseInt(root.maxDiskPercentText.trim(), 10)
    if (isNaN(pct) || pct < 1 || pct > 90) pct = 30
    var args = [
      "receive",
      "--mode", root.selectedMode,
      "--name", root.transferName.trim() || "Omarchy",
      "--directory", root.resolveDir(root.receivePath),
      "--ble-wake", root.bleWake ? "true" : "false",
      "--copy-clipboard", root.copyToClipboard ? "true" : "false",
      "--max-disk-percent", String(pct)
    ]
    if (root.contactsOnly) {
      args.push("--contacts-only")
    }
    if (root.selectedMode === "timed") {
      args.push("--seconds", String(root.selectedMinutes * 60))
    }
    root.runCtl(args)
  }

  function findPeers() {
    if (peersProc.running) return
    peersProc.running = true
  }

  function peerHost(peer) {
    var addr = String(peer.address || "")
    var m = addr.match(/^\[(.*)\]:(\d+)$/)
    return m ? m[1] : addr
  }

  function peerPort(peer) {
    var addr = String(peer.address || "")
    var m = addr.match(/^\[(.*)\]:(\d+)$/)
    return m ? m[2] : "8770"
  }

  function sendFiles() {
    if (root.selectedPeer < 0 || root.selectedPeer >= root.peers.length) return
    var peer = root.peers[root.selectedPeer]
    var args = [
      "send",
      "--host", root.peerHost(peer),
      "--port", root.peerPort(peer),
      "--name", root.transferName.trim() || "Omarchy"
    ]
    for (var i = 0; i < root.selectedFiles.length; i++) {
      args.push("--file", root.selectedFiles[i])
    }
    var link = root.linkText.trim()
    if (link.length > 0) args.push("--link", link)
    root.runCtl(args)
  }

  function validateSettings() {
    var name = root.transferName.trim()
    if (name.length === 0) return "Device name cannot be empty"
    if (unescape(encodeURIComponent(name)).length > 63) return "Device name must be at most 63 bytes"
    var dir = root.resolveDir(root.receivePath)
    if (dir.length === 0 || dir.charAt(0) !== "/") return "Save directory must be an absolute path"
    var pctRaw = root.maxDiskPercentText.trim()
    if (!/^\d+$/.test(pctRaw)) return "Max free disk % must be an integer (1-90)"
    var pct = parseInt(pctRaw, 10)
    if (pct < 1 || pct > 90) return "Max free disk % must be between 1 and 90"
    return ""
  }

  function saveSettings() {
    var err = root.validateSettings()
    root.settingsError = err
    if (err.length > 0) return
    var resolvedDir = root.resolveDir(root.receivePath)
    root.receivePath = resolvedDir
    var pct = parseInt(root.maxDiskPercentText.trim(), 10)
    var entry = { id: root.moduleName }
    for (var existing in root.settings) {
      if (existing !== "id") entry[existing] = root.settings[existing]
    }
    entry.name = root.transferName.trim()
    entry.directory = resolvedDir
    entry.defaultWindowMinutes = root.selectedMinutes
    entry.bleWake = root.bleWake
    entry.contactsOnly = root.contactsOnly
    entry.copyToClipboard = root.copyToClipboard
    entry.maxDiskPercent = pct
    root.settings = entry
    if (root.hostWidget && "settings" in root.hostWidget) root.hostWidget.settings = entry
    if (root.bar && root.bar.shell && typeof root.bar.shell.updateEntryInline === "function") {
      root.bar.shell.updateEntryInline(root.moduleName, entry)
    }
    root.settingsOpen = false
  }

  Process {
    id: subscribeProc
    running: true
    command: ["/usr/bin/ac-dc", "ctl", "subscribe"]
    stdout: SplitParser {
      onRead: function(line) { root.applySnapshot(line) }
    }
    onExited: {
      root.phase = "offline"
      reconnectTimer.restart()
    }
  }

  // One-shot backoff reconnect only if the stream disconnects unexpectedly.
  Timer {
    id: reconnectTimer
    interval: 2000
    repeat: false
    onTriggered: {
      if (!subscribeProc.running) subscribeProc.running = true
    }
  }

  Process {
    id: actionProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        try {
          var r = JSON.parse(String(text).trim())
          if (r && r.ok === false && r.message) {
            root.lastError = root.sanitizeText(r.message, 200)
          }
        } catch (e) {}
      }
    }
  }

  Process {
    id: peersProc
    command: ["/usr/bin/ac-dc", "ctl", "peers"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        try {
          var r = JSON.parse(String(text).trim())
          if (r && r.ok === false && r.message) {
            root.lastError = root.sanitizeText(r.message, 200)
          }
        } catch (e) {}
      }
    }
  }

  Process {
    id: fullUiProc
    command: ["/usr/bin/ac-dc", "ui"]
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.receiving ? "󰄀" : (root.sending ? "󰇚" : "≋")
    slotSize: Style.bar.statusSlot
    fontSize: Style.font.caption
    foreground: (root.phase === "error" || root.lastError.length > 0)
      ? Color.urgent
      : (root.phase === "offline"
        ? Qt.rgba(1, 1, 1, 0.45)
        : (root.bar ? root.bar.barForeground : Color.foreground))
    tooltipText: "Adhoc: " + root.statusSummary()
    onPressed: root.togglePopup()
  }

  PopupCard {
    id: popup
    anchorItem: root
    bar: root.bar
    owner: root
    open: root.popupOpen
    contentWidth: popup.fittedContentWidth(Style.space(390))
    contentHeight: popup.fittedContentHeight(scroller.height)

    Flickable {
      id: scroller
      width: parent.width
      height: Math.min(content.implicitHeight, Style.space(520))
      contentWidth: width
      contentHeight: content.implicitHeight
      clip: true
      interactive: contentHeight > height

      ColumnLayout {
        id: content
        width: scroller.width
        spacing: Style.space(10)

        RowLayout {
          Layout.fillWidth: true
          spacing: Style.space(9)
          BorderSurface {
            Layout.preferredWidth: Style.space(34)
            Layout.preferredHeight: Style.space(34)
            radius: Style.spacing.labelGap
            color: Style.normalFillFor(root.bar ? root.bar.foreground : Color.foreground, Color.accent)
            Text {
              anchors.centerIn: parent
              text: "≋"
              textFormat: Text.PlainText
              color: Color.accent
              font.family: root.font(Style.font.subtitle)
              font.pixelSize: Style.font.subtitle
            }
          }
          ColumnLayout {
            Layout.fillWidth: true
            spacing: Style.space(2)
            Text {
              text: "Adhoc Connection"
              textFormat: Text.PlainText
              color: Color.foreground
              font.family: root.font(Style.font.body)
              font.pixelSize: Style.font.body
              font.bold: true
            }
            Text {
              text: root.statusSummary()
              textFormat: Text.PlainText
              color: root.lastError.length > 0 ? Color.urgent : Qt.rgba(1, 1, 1, 0.62)
              elide: Text.ElideRight
              Layout.fillWidth: true
              font.family: root.font(Style.font.caption)
              font.pixelSize: Style.font.caption
            }
          }
          Button {
            text: root.settingsOpen ? "Close" : "Settings"
            onClicked: root.settingsOpen = !root.settingsOpen
          }
        }

        PanelSeparator { Layout.fillWidth: true }

        ColumnLayout {
          Layout.fillWidth: true
          spacing: Style.space(6)
          Button {
            Layout.fillWidth: true
            text: root.receiveButtonLabel()
            selected: root.receiving
            enabled: !root.busy && root.phase !== "offline"
            onClicked: root.toggleReceive()
          }
          RowLayout {
            Layout.fillWidth: true
            spacing: Style.space(6)
            visible: !root.receiving
            Button {
              Layout.fillWidth: true
              text: "10m"
              selected: root.selectedMode === "timed" && root.selectedMinutes === 10
              onClicked: { root.selectedMode = "timed"; root.selectedMinutes = 10 }
            }
            Button {
              Layout.fillWidth: true
              text: "30m"
              selected: root.selectedMode === "timed" && root.selectedMinutes === 30
              onClicked: { root.selectedMode = "timed"; root.selectedMinutes = 30 }
            }
            Button {
              Layout.fillWidth: true
              text: "Once"
              selected: root.selectedMode === "once"
              onClicked: root.selectedMode = "once"
            }
            Button {
              Layout.fillWidth: true
              text: "Always"
              selected: root.selectedMode === "always"
              onClicked: root.selectedMode = "always"
            }
          }
          RowLayout {
            Layout.fillWidth: true
            spacing: Style.space(6)
            enabled: !root.receiving
            Button {
              Layout.fillWidth: true
              text: "Everyone"
              selected: !root.contactsOnly
              onClicked: root.contactsOnly = false
            }
            Button {
              Layout.fillWidth: true
              text: "Contacts"
              selected: root.contactsOnly
              onClicked: root.contactsOnly = true
            }
          }
        }

        BorderSurface {
          visible: root.pendingTransfer !== null
          Layout.fillWidth: true
          Layout.preferredHeight: pendingCol.implicitHeight + Style.space(16)
          radius: Style.spacing.labelGap
          color: Style.normalFillFor(Color.accent, Color.accent)

          ColumnLayout {
            id: pendingCol
            anchors.fill: parent
            anchors.margins: Style.space(8)
            spacing: Style.space(6)
            Text {
              Layout.fillWidth: true
              textFormat: Text.PlainText
              color: Color.foreground
              font.family: root.font(Style.font.caption)
              font.pixelSize: Style.font.caption
              font.bold: true
              text: root.pendingTransfer
                ? ("Incoming from " + root.pendingTransfer.sender)
                : ""
            }
            Text {
              Layout.fillWidth: true
              textFormat: Text.PlainText
              color: Qt.rgba(1, 1, 1, 0.78)
              wrapMode: Text.WordWrap
              font.family: root.font(Style.font.caption)
              font.pixelSize: Style.font.caption
              text: root.pendingTransfer ? root.pendingTransfer.items.join(", ") : ""
            }
            RowLayout {
              Layout.fillWidth: true
              spacing: Style.space(6)
              Button {
                Layout.fillWidth: true
                text: "Accept"
                selected: true
                onClicked: root.runCtl(["approve"])
              }
              Button {
                Layout.fillWidth: true
                text: "Decline"
                onClicked: root.runCtl(["reject"])
              }
            }
          }
        }

        PanelSeparator { Layout.fillWidth: true }

        PeerSection {
          peers: root.peers
          selectedPeer: root.selectedPeer
          selectedFiles: root.selectedFiles
          linkText: root.linkText
          busy: root.busy
          sending: root.sending
          activeTransfer: root.activeTransfer
          fontFn: root.font
          onRefreshPeers: root.findPeers()
          onSelectPeer: function(idx) { root.selectedPeer = idx }
          onFilesDropped: function(urls) {
            var next = root.selectedFiles.slice()
            for (var i = 0; i < urls.length; i++) {
              var p = String(urls[i]).replace(/^file:\/\//, "")
              if (p.length > 0 && next.indexOf(p) < 0) next.push(p)
            }
            root.selectedFiles = next
          }
          onClearFiles: root.selectedFiles = []
          onLinkEdited: function(t) { root.linkText = t }
          onSendRequested: root.sendFiles()
          onCancelRequested: root.runCtl(["cancel"])
          onOpenFullUi: { if (!fullUiProc.running) fullUiProc.running = true }
        }

        RowLayout {
          visible: root.clipboardState !== null
          Layout.fillWidth: true
          spacing: Style.space(6)
          Text {
            text: "Clipboard"
            textFormat: Text.PlainText
            color: Color.foreground
            font.family: root.font(Style.font.caption)
            font.pixelSize: Style.font.caption
            font.bold: true
          }
          Text {
            Layout.fillWidth: true
            textFormat: Text.PlainText
            elide: Text.ElideRight
            color: (root.clipboardState && root.clipboardState.last_error)
              ? Color.urgent
              : Qt.rgba(1, 1, 1, 0.65)
            font.family: root.font(Style.font.caption)
            font.pixelSize: Style.font.caption
            text: {
              if (!root.clipboardState) return ""
              var s = root.sanitizeText(root.clipboardState.state || "idle", 32)
              if (root.clipboardState.last_item_kind) {
                return s + " · " + root.sanitizeText(root.clipboardState.last_item_kind, 48)
              }
              if (root.clipboardState.last_error) {
                return s + " · " + root.sanitizeText(root.clipboardState.last_error, 64)
              }
              return s
            }
          }
          Button {
            visible: root.clipboardState !== null && root.clipboardState.state !== "unpaired"
            text: (root.clipboardState && (root.clipboardState.state === "paused" || root.clipboardState.state === "error"))
              ? "Start"
              : "Pause"
            onClicked: {
              var paused = root.clipboardState && (root.clipboardState.state === "paused" || root.clipboardState.state === "error")
              root.runCtl([paused ? "clipboard_start" : "clipboard_stop"])
            }
          }
        }

        SettingsSection {
          visible: root.settingsOpen
          deviceName: root.transferName
          receiveDir: root.receivePath
          bleWake: root.bleWake
          copyToClipboard: root.copyToClipboard
          maxDiskPercentText: root.maxDiskPercentText
          validationError: root.settingsError
          fontFn: root.font
          onNameEdited: function(v) { root.transferName = v }
          onDirEdited: function(v) { root.receivePath = v }
          onBleWakeToggled: function(v) { root.bleWake = v }
          onCopyToClipboardToggled: function(v) { root.copyToClipboard = v }
          onMaxDiskPercentEdited: function(v) { root.maxDiskPercentText = v }
          onSaveRequested: root.saveSettings()
        }
      }
    }
  }
}
