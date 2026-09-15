#!/usr/bin/env bash
#
# awdl-capture.sh - capture a real Universal Clipboard / Continuity session on a
# Mac's awdl0 and extract everything useful for the Linux ac-dc port:
#   * the peer's mDNS service announcement (_companion-link._tcp: instance,
#     port, TXT rpBA/rpAD/rpVr, AAAA link-local) - validates discover.rs
#   * the companion-link TCP flow framing (sizes/direction) - validates the
#     Pair-Verify / OPACK framing in companion_client.rs (payload is encrypted,
#     but the handshake structure is visible)
#
# RUN ON macOS, as root:   sudo ./awdl-capture.sh
#
# The clipboard *content* is encrypted (ChaCha20 over companion-link), so this
# does NOT recover the copied text - it recovers the PROTOCOL, which is what we
# need. AWDL management frames (sync/channel-sequence TLVs) are NOT on awdl0;
# see the note at the end for a monitor-mode capture if we want those too.
set -u

DUR="${DUR:-45}"
OUT="${OUT:-$HOME/awdl-capture}"
PCAP="$OUT/awdl.pcap"
REPORT="$OUT/report.txt"

# --- locate tshark (Wireshark CLI) ------------------------------------------
TSHARK=""
for c in tshark /opt/homebrew/bin/tshark /usr/local/bin/tshark \
         "/Applications/Wireshark.app/Contents/MacOS/tshark"; do
    command -v "$c" >/dev/null 2>&1 && { TSHARK="$c"; break; }
    [ -x "$c" ] && { TSHARK="$c"; break; }
done
if [ -z "$TSHARK" ]; then
    echo "tshark not found. Install Wireshark (includes the AWDL dissector):" >&2
    echo "    brew install --cask wireshark      # or wireshark CLI: brew install wireshark" >&2
    exit 1
fi

[ "$(id -u)" -eq 0 ] || { echo "run as root:  sudo $0" >&2; exit 1; }
command -v tcpdump >/dev/null || { echo "tcpdump missing (unexpected on macOS)" >&2; exit 1; }
mkdir -p "$OUT"

echo "=============================================================================="
echo " AWDL / Universal Clipboard capture"
echo "=============================================================================="
echo
echo " BEFORE you continue, set up an ACTIVE AWDL session:"
echo "   1. On the Mac AND the iPhone: open AirDrop (Finder->AirDrop / Control"
echo "      Center) set to 'Everyone', keep both awake and close together."
echo "   2. Confirm awdl0 is up:  ifconfig awdl0 | grep -q 'status: active' && echo up"
echo
echo " DURING the ${DUR}s capture, generate real traffic:"
echo "   - COPY some text on the iPhone, then PASTE (Cmd-V) on the Mac  (Universal"
echo "     Clipboard -> a companion-link pull happens on awdl0)."
echo "   - Also AirDrop a small file Mac->iPhone to force an active session."
echo
read -r -p "Press Enter to start the ${DUR}s capture..." _

if ! ifconfig awdl0 >/dev/null 2>&1; then
    echo "WARNING: awdl0 not present - open AirDrop to bring it up, then re-run." >&2
fi

echo "capturing awdl0 for ${DUR}s -> $PCAP"
tcpdump -i awdl0 -w "$PCAP" -U >/dev/null 2>&1 &
TPID=$!
sleep "$DUR"
kill "$TPID" 2>/dev/null
wait "$TPID" 2>/dev/null

PKTS=$("$TSHARK" -r "$PCAP" 2>/dev/null | wc -l | tr -d ' ')
echo "captured $PKTS packets"

# --- build the report --------------------------------------------------------
{
echo "###### ac-dc awdl0 capture report ######"
echo "date: $(date)"; echo "packets: $PKTS"; echo
echo "== [1] mDNS: services announced over AWDL (companion-link etc.) =="
# Every mDNS record: name / type / SRV target+port / TXT / AAAA
"$TSHARK" -r "$PCAP" -Y "mdns" -T fields \
    -e dns.qry.name -e dns.resp.name -e dns.srv.name -e dns.srv.port \
    -e dns.srv.target -e dns.txt -e dns.aaaa -e dns.ptr.domain_name \
    -E occurrence=a 2>/dev/null | sort -u | grep -iE 'companion|_apple|_rdlink|local|rpBA|rpAD' | head -60
echo
echo "-- full mDNS decode of companion-link records (verbose) --"
"$TSHARK" -r "$PCAP" -Y "mdns && frame contains \"companion-link\"" -V 2>/dev/null | \
    sed -n '/Multicast Domain Name System/,/^Frame /p' | head -120
echo
echo "== [2] TCP conversations on awdl0 (companion-link = the biggest one) =="
"$TSHARK" -r "$PCAP" -q -z conv,tcp 2>/dev/null | head -25
echo
echo "== [3] companion-link framing: first payload-bearing TCP packets =="
"$TSHARK" -r "$PCAP" -Y "tcp.len>0" -T fields \
    -e frame.number -e tcp.stream -e ipv6.src -e ipv6.dst -e tcp.srcport \
    -e tcp.dstport -e tcp.len 2>/dev/null | head -40
echo
echo "-- hex of the first few companion-link payloads (framing/handshake) --"
for s in 0 1 2; do
  echo "--- tcp.stream $s, first client->server payload ---"
  "$TSHARK" -r "$PCAP" -Y "tcp.stream==$s && tcp.len>0" -T fields -e data.data 2>/dev/null | head -3
done
echo
echo "== [4] AWDL data-plane peers seen (ND / IPv6 link-locals on awdl0) =="
"$TSHARK" -r "$PCAP" -Y "ipv6" -T fields -e ipv6.src -e ipv6.dst 2>/dev/null | tr '\t' '\n' | sort -u | grep -i '^fe80' | head
echo
echo "###### end report ######"
} > "$REPORT" 2>&1

echo
echo "Report written to: $REPORT"
echo "PCAP written to:    $PCAP  (keep it; we can dig deeper)"
echo
echo "NEXT: paste the CONTENTS of $REPORT back into the chat:"
echo "      cat \"$REPORT\""
echo
echo "------------------------------------------------------------------------------"
echo "OPTIONAL - AWDL *management* frames (sync params / channel sequence), which"
echo "live on the radio, not awdl0. This drops Wi-Fi briefly (monitor mode):"
echo "   sudo /System/Library/PrivateFrameworks/Apple80211.framework/Versions/Current/Resources/airport en0 sniff 6"
echo "   # Ctrl-C after ~20s; it writes /tmp/airportSniffXXXX.pcap; then:"
echo "   $TSHARK -r /tmp/airportSniff*.pcap -Y awdl -V | less   # look for AWDL sync/chanseq/election TLVs"
echo "------------------------------------------------------------------------------"
