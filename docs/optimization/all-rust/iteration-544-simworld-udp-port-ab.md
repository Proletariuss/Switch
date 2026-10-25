# Iteration 544 — SIM World UDP source-port A/B

2026-10-18. The retired Go SWu transport used an unconnected UDP socket and
accepted replies from the selected ePDG IP even when their source port
changed. The Rust transparent transport uses a connected UDP socket, which
macOS filters to one remote IP:port. This was a plausible, testable difference
behind the zero inbound packets observed after SIM World CHILD SA setup.

An isolated experiment changed only Rust's transparent socket to `send_to` /
`recv_from`, retained the proxy-selected ePDG IP and local source port, and
discarded packets from other source IPs. Focused loopback coverage showed that
a reply from the same IP but a changed port reached the transport; the VoWiFi
crate's 277 tests passed. The experimental Release daemon then connected to
ESTKme-RED's active SIM World 454/03 profile through the existing Surge
policy. IKE/EAP and CHILD SA succeeded, but IMS `plain_connect` still timed
out: ESP sent 8, received 0, rejected 0; post-CHILD IKE, keepalive and invalid
datagram counts also remained zero. The sanitized probe result is
`build/reports/iteration-544-simworld-udp-source-sanitized.json`.

The experiment therefore did not improve the real fault. The socket change and
its experimental tests were removed, and the normal connected-socket Release
daemon was rebuilt from the restored source. The probe stopped VoWiFi,
disconnected the reader, exited the daemon and removed its isolated data
directory. It did not switch profiles, download eSIMs, send SMS, place calls,
change Surge rules or generate a DMG.

This rules out *connected-socket source-port filtering alone* as the cause in
this observed network session. The remaining uncertainty is whether ESP
reached the proxy exit/ePDG and, if so, whether the ePDG rejected the ESP or
inner traffic. The next decisive evidence is a controlled known-good client
comparison on the same card/network or a packet-class counter at the proxy
exit; repeating local 8/0 probes without that evidence has low value.
