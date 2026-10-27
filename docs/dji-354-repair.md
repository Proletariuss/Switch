# DJI-Modul: Rust-Reparatur für den Kandidaten 3.54

## Ziel und Ausgangsbasis

Ziel sind nachvollziehbare Quellen und ein stabiler Modulbetrieb, auch nach
vorübergehender Belegung, abgebrochenen UI-Anfragen und Verbindungswechseln.
Ausgangspunkt ist 3.53, Commit `e964afd85b6c2187a409d40919c2c131ccf05042`.
Die Implementierung bleibt im nativen Rust-Backend; der Go-Daemon wird nicht
wieder eingeführt. Dieser Auftrag erzeugt eine CI-App, keinen neuen DMG-Release.

## Beobachtungen und Grenzen

Die bereitgestellte Diagnose wurde lokal ausschließlich lesend ausgewertet.
Sie bestätigt Version 3.53 und den veröffentlichten Quell-Digest. Im Zeitraum
von ungefähr 305 Sekunden gab es 152 Snapshot-Antworten mit HTTP 200,
drei USB-Netzmodus-Anfragen mit HTTP 409 und drei SMS-Aktualisierungen mit HTTP 502.
HTTP 200 belegt die Snapshot-Anfrage, nicht vollständige Modem- oder SIM-Bereitschaft.
Die damaligen Logs enthalten keine ausreichende Eigentümer-/Freigabespur, um
den genauen Auslöser dieser Gerätesitzung nachträglich zu beweisen.
Rohdiagnosen, Teilnehmerkennungen und Zugangsdaten werden nicht veröffentlicht.

## Verhaltensreferenzen

Die lokal bereitgestellte Switch-2.67-Quelle und
[DJOneHubNative 7194ae8](https://github.com/cr-zhichen/DJOneHubNative/tree/7194ae8cd71da34e0ab3727874dafb82ffd2537b)
dienen als Verhaltensreferenzen. Die DJI-Mac-Verbindung verwendet USB AT;
QMI ist keine Voraussetzung für die grundlegenden AT-Identitätsabfragen.
USB-Modus, Funkzustand, SIM-Bereitschaft und nutzbare Netzwerkverbindung sind
getrennte Zustände. USB-Netzmodus 0/1 ist nicht gleich Flugmodus.
Lizenzgebundener Referenzcode wird nicht übernommen.

## Korrigierte Lebensdauer

Eine gewöhnliche Hintergrundabfrage des Sprachstatus konnte eine eSIM-
Reservierung überlappen. Die bisherige Freigabe verbrauchte den einzigen
Reservierungshandle, konnte wegen des temporären Pins mit busy scheitern und
ließ danach eine dauerhaft blockierende Reservierung zurück.
Die vorhandene alte Warmup-Fixture dokumentierte dieses blockierte Ende,
prüfte jedoch keine spätere Wiederherstellung.

Die Freigabepflicht bleibt jetzt in einem eigenen, beim Drain gezählten Worker
erhalten. Sie wartet auf das tatsächliche Ende der ursprünglichen Arbeit.
Abbruch des Wartenden oder eine volle Job-Warteschlange verlieren den Handle
nicht. Producer, Epoch, Token und Sequenz verhindern die Freigabe eines Nachfolgers.
Auch die Akquisition ist an die ursprünglich erfasste Bindung gebunden.
Gewöhnlicher Status darf die eSIM-Reservierung nicht mehr überlappen;
bereits erforderliche Cleanup-Arbeit und CallMode-Lesekompatibilität bleiben erhalten.

Sieben gezielte Cleanup-Tests prüfen Belegung, Abbruch, Shutdown, Retirement,
gleichnamige Producer über Epoch-Wechsel und Diagnosegrenzen.
Die Startup-Fixture führt reale Rust-Transport- und HTTP-Pfade aus:
CCHC-Abschluss, HTTP 409 ohne USB-Netzschreibbefehl, tatsächliche Freigabe,
SMS-/Metadatenbereitschaft und HTTP 200 mit genau einem USB-Netzschreibbefehl.
`needs_reboot=true` bleibt erhalten; dies behauptet weder Neustart noch Internetzugang.

## Begrenztes Warten auf interne Leseabfragen

Auch normale Metadatenabfragen und der Startup-Warmup reservieren kurzzeitig den
SIM-Kanal. Nur diese beiden internen Pfade erhalten einen privaten Lesemarker;
HTTP-Anfragen können ihn nicht setzen. Der Netzmoduswechsel wartet innerhalb
seiner bestehenden absoluten 25-Sekunden-Frist auf deren Freigabe, ohne Actor-Slot
oder Gate festzuhalten. Danach wird dieselbe Gerätebindung erneut geprüft.
Normale eSIM-Schreibvorgänge, Anrufe, Pins und unklare Ergebnisse werden weiterhin
nicht verdrängt. Ein Timeout erzeugt keinen Schreibbefehl.

Sechs zusätzliche Grenztests prüfen Freigabe, einmaliges Schreiben und
dauerhafte Antwortwiederholung, unveränderte echte Konflikte, nicht fälschbare
Markierung, Timeout, Retirement und HTTP-Abbruch ohne Wiederholung.
Eine weitere Startup-Fixture verbindet den tatsächlichen CCHC-Abschluss mit
einer wartenden HTTP-Anfrage und genau einem späteren USB-Netzschreibbefehl.

## AT-Antwortgrenzen und Diagnose

Statusabfragen verlangen eine vollständige terminale Antwortzeile.
Nachfolgende URCs bleiben im getrennten Capture erhalten. Nicht abgeschlossene
Antworten stoppen den Statusplan und dürfen keiner Folgeabfrage zugerechnet werden.
Die gewöhnliche Partial-Response-Kompatibilität außerhalb des Statuspfades,
SMS-Prompt/ESC-Behandlung und abschließende Kartenbindungskontrolle bleiben erhalten.
Fünf neue Byte-Transkript-Tests ergänzen vorhandene Transport- und Statustests:
Fragmentierung, späte Antworten, ERROR, SIM-PIN, URCs, Kartenwechsel und Abbruch.

Modulereignisse verwenden ein geschlossenes Schema für Phase, Ergebnis,
Fehlerklasse, Transport und begrenzte Eigentümer-/Pin-Zähler.
Wiederholungen werden begrenzt zusammengefasst; Fortschritt und Fehlerwechsel
bleiben sichtbar. AT-Inhalte, Identitäten, Token, Pfade und freie Fehlertexte
werden nicht übernommen. Swift exportiert nur zulässige Felder und verwirft
unbekannte Einzelereignisse, ohne den gesamten Diagnoseblock zu verlieren.

## Review und Softwareabnahme

Drei Quellprüfrunden fanden vor der Cloud-Normalisierung statt:
1. Getrennte Rust-Lebenszyklus-, Referenz-/Framing- und Diagnoseprüfungen.
2. Kreuzprüfung von Freigabe, Protokollgrenzen und Cloud-Vorbereitung.
3. Integrierte Prüfung von `9b50c00e`, HTTP-Regressionspfad und verbleibenden Grenzen.
Dabei wurden zusätzlich die Akquisition über einen Epoch-Wechsel und die
begrenzte, versionskonsistente Cloud-Vorbereitung korrigiert.
Die anschließende Lesewarte-Erweiterung in `22aad7fa` wurde separat auf
Fristen, Gerätebindung, Abbruch, Konfliktschutz und HTTP-Einmaligkeit kreuzgeprüft.

Eine erste Cloud-Prüfung deckte außerdem eine bestehende Race Condition der
Swift-Test-Fixture auf: feste Scheduler-Yields konnten denselben Request zweimal
abschließen. Die beiden betroffenen Testdateien warten nun kausal auf einen neuen,
pfadgenauen Request und das Ende der tatsächlichen Store-Tasks; die strengen
Einmaligkeits- und Negativassertionen bleiben erhalten. Nur Tests wurden geändert.
Die unabhängig geprüfte Änderung in `23ee8748` bestand
[fünf gezielte Wiederholungen und die vollständige Swift-Models-Suite](https://github.com/Proletariuss/Switch/actions/runs/40420848246).
Testseitige Reflection bindet die Barrieren an die aktuellen privaten Lifecycle-Felder;
eine Änderung dieser Felder muss ausdrücklich auch die Fixture aktualisieren.

Die anschließende vollständige Rust-Prüfung führte 3.747 reguläre Daemon-Tests
aus: 3.743 bestanden, vier Fixture-Assertierungen scheiterten. Die Korrektur
bleibt auf Tests beschränkt: Call-Belegung wird erst nach einer echten,
verifizierten CFUN-Beobachtung geprüft; Byte-Fixtures behalten die native
SMS-Policy und prüfen ihre Sperre ausdrücklich. Der bisherige Refresh-Test
für eine verlorene Freigabe prüft jetzt den erhaltenen Worker: HTTP pending,
tatsächliche Pin-Freigabe, Abschluss derselben Operation und kein erneutes
CCHO/CGLA/CCHC-Lesen. Alle vier Fehler bleiben als fehlgeschlagener
[Cloud-Lauf](https://github.com/Proletariuss/Switch/actions/runs/40421179618)
nachvollziehbar. `12872514` und `7f28de8e` wurden unabhängig kreuzgeprüft;
der endgültige Stand muss die unveränderten vollständigen Gates erneut bestehen.

Die verbindlichen Testergebnisse, der endgültige Commit und das App-Artefakt
werden im zugehörigen PR und seinen GitHub-Läufen festgehalten.
Erforderlich bleiben die unveränderten gemeinsamen Rust-/Swift-/App-Gates,
beide RustSec-Prüfungen und die Review-Policy-Fixtures.
Formatierung, Tests und Builds erfolgen ausschließlich auf GitHub-Runnern.

Software-Fixtures belegen keine reale DJI-/SIM-Abnahme, Mobilfunkregistrierung,
SMS-Zustellung, Internetverbindung, Audioqualität, macOS-27-Laufzeit oder
24-/72-Stunden-Stabilität. Diese Grenzen bleiben ausdrücklich offen.
Veröffentlichte Tags und Pakete 3.51–3.53 bleiben unverändert.
