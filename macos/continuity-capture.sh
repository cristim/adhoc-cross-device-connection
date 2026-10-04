#!/usr/bin/env bash
#
# continuity-capture.sh - capture a real Universal Clipboard / Handoff session on
# a Mac and extract everything useful for the Linux ac-dc port.
#
# RUN ON macOS, as root:   sudo ./continuity-capture.sh
#
# Why not just awdl0: Continuity's companion-link (Rapport) picks a transport per
# session. When the Mac and the iPhone share an IP subnet it uses the infra Wi-Fi
# interface (en0) and awdl0 stays empty; llw0 also carries Continuity traffic.
# So we capture EVERY interface at once via pktap, which tags each packet with
# the interface name and the originating process - that tells us which link and
# which daemon actually carried the session instead of guessing.
#
# We also pull the matching window out of the unified log. That is where the
# transport decision ("via Rapport"), the pasteboard advertise/request timeline,
# and - most valuable for this project - the raw BLE Handoff advertisement bytes
# (Apple mfgData TLV 0x0c) are visible. Those bytes are ground truth for
# validating src/gcm.rs, which README.md currently lists as unvalidated.
#
# Clipboard *content* stays encrypted; this recovers the PROTOCOL and the crypto
# inputs, not the copied text.
set -u

DUR="${DUR:-45}"
OUT="${OUT:-$HOME/continuity-capture}"
PCAP="$OUT/continuity.pcap"
LOGTXT="$OUT/unified.log"
REPORT="$OUT/report.txt"
ADVERTS="$OUT/handoff-adverts.txt"

# --- locate tshark (Wireshark CLI) ------------------------------------------
TSHARK=""
for c in tshark /opt/homebrew/bin/tshark /usr/local/bin/tshark \
         "/Applications/Wireshark.app/Contents/MacOS/tshark"; do
    command -v "$c" >/dev/null 2>&1 && { TSHARK="$c"; break; }
    [ -x "$c" ] && { TSHARK="$c"; break; }
done
if [ -z "$TSHARK" ]; then
    echo "tshark not found. Install Wireshark (includes the AWDL dissector):" >&2
    echo "    brew install --cask wireshark" >&2
    exit 1
fi

[ "$(id -u)" -eq 0 ] || { echo "run as root:  sudo $0" >&2; exit 1; }
command -v tcpdump >/dev/null || { echo "tcpdump missing (unexpected on macOS)" >&2; exit 1; }
mkdir -p "$OUT"

# --- preflight: where are we, and will AWDL even be used? -------------------
MYIP="$(ipconfig getifaddr en0 2>/dev/null || true)"
SUBNET="${MYIP%.*}"

echo "=============================================================================="
echo " Continuity / Universal Clipboard capture (all interfaces)"
echo "=============================================================================="
echo
echo " Mac en0 address: ${MYIP:-<none>}"
for i in awdl0 llw0 en0; do
    if ! ifconfig "$i" >/dev/null 2>&1; then st="absent"
    else st="$(ifconfig "$i" 2>/dev/null | awk '/status:/{print $2; f=1} END{if(!f) print "up (no status line)"}')"; fi
    printf '   %-6s %s\n' "$i" "$st"
done
echo
if [ -n "$SUBNET" ] && arp -an | grep -q "($SUBNET\."; then
    echo " NOTE: other hosts are on your $SUBNET.0/24 LAN. If the iPhone is one of"
    echo " them, Rapport will use en0 and awdl0 will stay EMPTY. That is normal and"
    echo " this script now captures it correctly - but if you specifically want the"
    echo " AWDL path (the one the Linux port has to reimplement), see FORCE AWDL"
    echo " at the end of this script's output."
    echo
fi
echo " BEFORE you continue:"
echo "   1. On the Mac AND the iPhone: open AirDrop set to 'Everyone', keep both"
echo "      awake and close together."
echo "   2. Make sure Handoff is on: System Settings > General > AirDrop & Handoff."
echo
echo " DURING the ${DUR}s capture, generate real traffic in BOTH directions:"
echo "   - COPY text on the iPhone, then PASTE (Cmd-V) on the Mac."
echo "   - COPY text on the Mac, then PASTE on the iPhone."
echo "   - Optionally AirDrop a small file Mac->iPhone."
echo
read -r -p "Press Enter to start the ${DUR}s capture..." _

START="$(date '+%Y-%m-%d %H:%M:%S')"

# pktap,all = every interface in one capture, each packet carrying the interface
# name and owning process. Needs root; there is no per-interface guessing left.
echo "capturing ALL interfaces for ${DUR}s -> $PCAP"
tcpdump -i pktap,all -s 0 -w "$PCAP" -U >/dev/null 2>&1 &
TPID=$!
sleep 1
if ! kill -0 "$TPID" 2>/dev/null; then
    echo "pktap capture failed to start; falling back to awdl0+llw0+en0" >&2
    tcpdump -i awdl0 -s 0 -w "$OUT/awdl0.pcap" -U >/dev/null 2>&1 &
    tcpdump -i llw0  -s 0 -w "$OUT/llw0.pcap"  -U >/dev/null 2>&1 &
    tcpdump -i en0   -s 0 -w "$OUT/en0.pcap"   -U >/dev/null 2>&1 &
    TPID=""
fi
sleep "$DUR"
if [ -n "$TPID" ]; then
    kill "$TPID" 2>/dev/null; wait "$TPID" 2>/dev/null
else
    pkill -f 'tcpdump -i (awdl0|llw0|en0)' 2>/dev/null
    sleep 1
    # the report reads a single $PCAP; merge the per-interface files into it
    MERGECAP="$(command -v mergecap || echo /Applications/Wireshark.app/Contents/MacOS/mergecap)"
    if [ -x "$MERGECAP" ]; then
        "$MERGECAP" -w "$PCAP" "$OUT"/awdl0.pcap "$OUT"/llw0.pcap "$OUT"/en0.pcap 2>/dev/null
    else
        echo "mergecap missing; per-interface pcaps left in $OUT, report will be partial" >&2
        cp "$OUT/en0.pcap" "$PCAP" 2>/dev/null
    fi
fi

END="$(date '+%Y-%m-%d %H:%M:%S')"

PKTS="$("$TSHARK" -r "$PCAP" 2>/dev/null | wc -l | tr -d ' ')"
echo "captured $PKTS packets"

# --- the unified log for exactly that window --------------------------------
# log show reads the persisted archive, so debug-level lines are included
# without having to run a firehose stream during the capture.
echo "extracting unified log for $START .. $END"
/usr/bin/log show --start "$START" --end "$END" --style compact --predicate '
    process == "useractivityd" OR process == "sharingd" OR process == "rapportd"
    OR process == "identityservicesd" OR process == "pboard" OR process == "bluetoothd"
' > "$LOGTXT" 2>/dev/null

# --- build the report --------------------------------------------------------
{
echo "###### ac-dc continuity capture report ######"
echo "date: $(date)"
echo "window: $START .. $END"
echo "packets: $PKTS"
echo "first frame protocols: $("$TSHARK" -r "$PCAP" -c1 -T fields -e frame.protocols 2>/dev/null | head -1)"
echo "  (expect a leading 'darwin' - Apple's tcpdump writes pcapng with the"
echo "   interface and process in the block options)"
if [ "$PKTS" = "0" ]; then
    echo
    echo "!! ZERO packets. Either the capture failed, or tshark cannot read DLT_PKTAP."
    echo "!! Check: $TSHARK -r $PCAP   and re-run with DUR=90 if the session was idle."
fi
echo

echo "== [0] which INTERFACE carried traffic (this is the question awdl0-only capture could not answer) =="
"$TSHARK" -r "$PCAP" -T fields -e frame.interface_name 2>/dev/null | sort | uniq -c | sort -rn | head -20
echo

echo "== [1] which PROCESS sent/received it (rapportd = companion-link, sharingd = AirDrop/Handoff) =="
"$TSHARK" -r "$PCAP" -T fields -e frame.darwin.process_info.pname 2>/dev/null | sort | uniq -c | sort -rn | head -20
echo

echo "== [2] Continuity daemons only: interface x process x peer =="
"$TSHARK" -r "$PCAP" -Y 'frame.darwin.process_info.pname in {"rapportd","sharingd","useractivityd","identityservicesd"}' \
    -T fields -e frame.interface_name -e frame.darwin.process_info.pname -e ip.src -e ip.dst -e ipv6.src -e ipv6.dst \
    -e tcp.srcport -e tcp.dstport 2>/dev/null | sort | uniq -c | sort -rn | head -30
echo

echo "== [3] mDNS service announcements, per interface (companion-link discovery data for discover.rs) =="
"$TSHARK" -r "$PCAP" -Y 'mdns' -T fields \
    -e frame.interface_name -e dns.qry.name -e dns.resp.name -e dns.srv.name -e dns.srv.port \
    -e dns.srv.target -e dns.txt -e dns.aaaa -e dns.ptr.domain_name \
    -E occurrence=a 2>/dev/null | sort -u | grep -iE 'companion|rdlink|rpBA|rpAD|rpVr' | head -40
echo
echo "-- which Rapport service name is actually in use --"
for svc in _rdlink._tcp _companion-link._tcp; do
    printf '   %-22s %s frames\n' "$svc" \
        "$("$TSHARK" -r "$PCAP" -Y "frame contains \"${svc%%._*}\"" 2>/dev/null | wc -l | tr -d ' ')"
done
echo
echo "-- verbose decode of any _companion-link._tcp record --"
"$TSHARK" -r "$PCAP" -Y 'mdns && (frame contains "rdlink" || frame contains "companion-link")' -V 2>/dev/null | \
    sed -n '/Multicast Domain Name System/,/^Frame /p' | head -120
echo

echo "== [4] TCP conversations =="
echo "-- Rapport/companion-link only (link-local IPv6; this is the session that matters) --"
"$TSHARK" -r "$PCAP" -q -z conv,tcp,'frame.darwin.process_info.pname=="rapportd"' 2>/dev/null | head -15
echo
echo "-- everything else, for context (mostly ordinary browsing) --"
"$TSHARK" -r "$PCAP" -q -z conv,tcp 2>/dev/null | head -15
echo

echo "== [5] companion-link framing =="
echo "wire format: TYPE(1) | LENGTH(3, big-endian) | encrypted body"
echo
RSTREAM="$("$TSHARK" -r "$PCAP" -Y 'frame.darwin.process_info.pname=="rapportd" && tcp' \
           -T fields -e tcp.stream 2>/dev/null | sort -un | head -1)"
if [ -z "$RSTREAM" ]; then
    echo "(no rapportd TCP session in this capture - the clipboard sync did not run,"
    echo " or the daemons were renamed; check section [7] for what the log saw)"
else
echo "rapportd tcp.stream=$RSTREAM"
echo
echo "-- packet ledger: request ~176-181B, response ~1.8kB is the normal shape --"
"$TSHARK" -r "$PCAP" -Y "tcp.stream==$RSTREAM && tcp.len>0" -T fields \
    -e frame.number -e frame.time_relative -e ipv6.src -e tcp.srcport -e tcp.dstport -e tcp.len \
    2>/dev/null | head -40
echo
echo "-- header of each PDU-initial segment: TYPE + 3-byte BE length --"
echo "   (header+declared length must equal the PDU's wire bytes; if it does not,"
echo "    the framing in companion_client.rs is wrong)"
"$TSHARK" -r "$PCAP" -Y "tcp.stream==$RSTREAM && tcp.len>0" -T fields \
    -e frame.number -e tcp.len -e tcp.payload 2>/dev/null | \
while IFS="$(printf '\t')" read -r fn tlen pay; do
    h="$(echo "$pay" | tr -d ':' | cut -c1-8)"
    [ "${#h}" -eq 8 ] || continue
    case "$h" in
        08*) printf '   frame %-6s seglen=%-6s hdr=%s  type=0x%s declared=%s\n' \
                 "$fn" "$tlen" "$h" "${h:0:2}" "$((16#${h:2:6}))" ;;
    esac
done | head -30
fi
echo
echo "== [6] link-local / peer addresses seen =="
"$TSHARK" -r "$PCAP" -T fields -e ipv6.src -e ipv6.dst 2>/dev/null | tr '\t' '\n' | sort -u | grep -i '^fe80' | head -20
echo

echo "== [7] transport decision + pasteboard timeline (unified log) =="
echo "-- transport actually chosen (Rapport = companion-link, the thing to reimplement) --"
grep -oiE 'via (Rapport|AWDL|BT|WiFi)[A-Za-z]*' "$LOGTXT" | sort | uniq -c | sort -rn
echo
echo "-- OUTBOUND: this Mac PULLING the remote pasteboard (the M2 client sequence) --"
echo "   note the two phases: type info first, then the data itself"
grep -iE 'pasteboard-fetch|\[REMOTE GEN\]|Requesting payload from|Requesting Handoff payload|Pasteboard types fetch requested|Received pasteboard info' "$LOGTXT" \
    | head -30
echo
echo "-- INBOUND: this Mac SERVING its pasteboard to the iPhone --"
grep -iE 'Received Handoff payload request|Responding to Handoff payload request|Types Requested from remote|Paste V2 Requested' "$LOGTXT" \
    | head -20
echo
echo "-- local pasteboard generations + what got advertised over BLE --"
grep -iE 'Got type update from client, gen:|ADVERTISING:|UAPasteboardAvailable' "$LOGTXT" | head -20
echo

echo "== [8] BLE Handoff advertisements (Apple mfgData TLV 0x0c) - ground truth for src/gcm.rs =="
echo "framing: 4c00 | 0c | LEN | STATUS(1) | CTR(2 LE) | TAG(1) | CIPHERTEXT(10)"
echo "status 08 = clipboard data available; 00 = nothing to pull."
echo
# bluetoothd logs both the adverts this Mac TRANSMITS (CBStackBLEAdvertiser
# "mfgData:") and the ones CoreBluetooth OBSERVES ("kCBAdvDataAppleMfgData").
# Split them: outgoing adverts are this Mac's own key+counter, observed ones are
# what a Linux ac-dc scanner would actually have to decrypt.
extract_adverts() {   # stdin -> normalised, deduped 0x0c TLVs
    grep -oiE '4c000c[0-9a-f]{30,}' | tr 'A-F' 'a-f' | \
    perl -ne 'chomp; my $l=hex(substr($_,6,2)); print substr($_,0,8+$l*2),"\n" if length($_)>=8+$l*2;' | \
    sort -u
}
show_adverts() {      # stdin -> annotated table, clipboard-available first
    awk '{ printf "%s  len=%s status=%s ctr=%s tag=%s ct=%s\n", $0, \
           substr($0,7,2), substr($0,9,2), substr($0,11,4), substr($0,15,2), substr($0,17,20) }' | \
    sort -k3,3r
}

grep -i 'mfgData:' "$LOGTXT"             | extract_adverts > "$OUT/adverts-outgoing.txt"
grep -i 'kCBAdvDataAppleMfgData' "$LOGTXT" | extract_adverts > "$OUT/adverts-observed.txt"
cat "$OUT/adverts-outgoing.txt" "$OUT/adverts-observed.txt" | sort -u > "$ADVERTS"

echo "-- OBSERVED by CoreBluetooth (what a Linux scanner sees; $(wc -l < "$OUT/adverts-observed.txt" | tr -d ' ') unique) --"
show_adverts < "$OUT/adverts-observed.txt" | head -25
echo
echo "-- TRANSMITTED by this Mac (own key+counter; $(wc -l < "$OUT/adverts-outgoing.txt" | tr -d ' ') unique) --"
show_adverts < "$OUT/adverts-outgoing.txt" | head -25
echo
echo "Sanity check: within one advertiser the ctr field must increase monotonically"
echo "when read little-endian. If it does, the CTR byte order in src/gcm.rs is right."
echo
echo "Validate the M1 crypto path against a clipboard-available advert:"
BEST="$(awk 'substr($0,9,2)=="08"' "$ADVERTS" | head -1)"
if [ -n "$BEST" ]; then
    echo "    ac-dc decrypt --keys keys.json --data ${BEST#4c00}"
else
    echo "    (no status=08 advert captured - copy something during the next run)"
fi
echo "Expected plaintext: STATUS(1) | ACTIVITY_HASH(7) | FLAGS(1) | UNUSED(1), flag 0x08 set."
echo
echo "(all unique adverts: $(wc -l < "$ADVERTS" | tr -d ' '), full list in $ADVERTS)"
echo
echo "###### end report ######"
} > "$REPORT" 2>&1

echo
echo "Report:  $REPORT"
echo "PCAP:    $PCAP"
echo "Log:     $LOGTXT"
echo "Adverts: $ADVERTS"
echo
echo "NEXT: paste the report back into the chat:"
echo "      cat \"$REPORT\""
echo
echo "------------------------------------------------------------------------------"
echo "FORCE AWDL (if section [0] shows en0 carried everything and you want the AWDL"
echo "path instead): the two devices must NOT share an IP subnet, while the iPhone"
echo "keeps its Wi-Fi radio alive for AWDL."
echo "   - On the iPhone use the CONTROL CENTRE Wi-Fi toggle, NOT Settings > Wi-Fi."
echo "     Control Centre disconnects from the network but leaves the radio up for"
echo "     AirDrop/AWDL; Settings turns the radio off and AWDL dies with it."
echo "   - Leave the Mac on its normal network, re-run this script."
echo "   - Also turn OFF Personal Hotspot: with it on, the devices can end up on a"
echo "     shared subnet again and Rapport goes back to the infra path."
echo
echo "OPTIONAL - AWDL *management* frames (sync params / channel sequence) live on"
echo "the radio, not on awdl0. This drops Wi-Fi briefly (monitor mode):"
echo "   sudo /System/Library/PrivateFrameworks/Apple80211.framework/Versions/Current/Resources/airport en0 sniff 6"
echo "   $TSHARK -r /tmp/airportSniff*.pcap -Y awdl -V | less"
echo
echo "OPTIONAL - un-redact the <private> fields in the log (device names, payload"
echo "identifiers). Needs the Apple 'Enable-Private-Data' logging profile; without"
echo "it sections [7]/[8] still work, you just see <private> in places."
echo "------------------------------------------------------------------------------"
