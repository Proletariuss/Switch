# DJI-Modul: Referenzvergleich und Wiederanlauf

Stand der Untersuchung: 27.10.2026. Dies ist ein Fehlerbericht mit Abnahmeplan,
keine Gerätefreigabe.

## Untersuchte Quellen

- Switch 3.55: `773a7fc8f7b576ed3eb16d9f437161ab9b7b8bbd`.
- DJOneHubNative: [`7194ae8`](https://github.com/cr-zhichen/DJOneHubNative/tree/7194ae8cd71da34e0ab3727874dafb82ffd2537b).
- Das vom Betreiber bereitgestellte Quellarchiv mit `VERSION=2.67` wurde
  tatsächlich gelesen. Keine Ausführung des alten Go-Backends.
- Benutzerdiagnosen wurden nur lokal gelesen. Keine vollständigen Diagnosen,
  Geräteidentitäten, Zugangsdaten oder Benutzerverzeichnisse werden veröffentlicht.

SHA-256 der untersuchten Dateien aus dem 2.67-Archiv, relativ zu
`backend/cmd/proletarius-macos/`:

| Datei | SHA-256 |
| --- | --- |
| main.go | 04c1a4c11f0b65cf9eeba787e906ce99f6f873ef7e14da29b8dc39c0fa2ad62b |
| call_mode.go | 89abd63bde8691978a288d66ca2b2405e377304bf66d1aa793e3c01d578988ba |
| call_mode_backup.go | b3e5642574f22496cab1366b4d1ca6e48b5cd0a6e53f1bf9c49ab3796c7bd2a6 |
| communication_shutdown.go | e3f13f555aec895cfe2198b0bc01fdccd3ce4b42ce4405b6108384efab6084a3 |

DJOneHubNative steht unter PolyForm Noncommercial 1.0.0 mit dem dortigen
Required Notice. Verglichen werden Verhalten, Reihenfolge und Protokoll; diese
Untersuchung integriert keinen Go-Prozess und übernimmt keinen fremden
Quelltext ohne Beachtung seiner Lizenz.

## Verhalten im Vergleich

| Bereich | Switch 2.67 / DJOneHubNative | Rust-Ausgangsstand | Folgerung |
| --- | --- | --- | --- |
| Grundverbindung | Serielles AT zuerst, USB-AT als Alternative; fehlendes Gerät verhindert nicht den späteren Wiederanschluss | Entsprechender Ablauf in `module_startup.rs` vorhanden, wird bei globaler Modulquarantäne aber nicht gestartet | Vorrangig die Zulassung und Wiederherstellung untersuchen, nicht AT/QMI neu erfinden |
| USB-Endpunkte | Herstellerklasse ff/00/00, AT-Kandidaten 2/3; ungeeignete Diagnose-/NMEA-Endpunkte vermeiden | Unterstützt zusätzlich erkannte QDC507-RNDIS-Kompositionen auf 4/5 | Die zusätzlichen geprüften Kompositionen nicht pauschal entfernen |
| Netzwerkmodus | Nur usbnet 0/1; AT-Konfiguration und erforderlicher Neustart sind getrennt | Gleiche Kommandos; zusätzlich Bindung, Reservation und dauerhafte Neustartbeobachtung | Kommandoannahme darf keine bestätigte Wiederverbindung vortäuschen |
| QMI | Optionale Erweiterung, nicht der normale macOS-AT-Startpfad | Ebenfalls optional; bei Modulquarantäne gesperrt | Fehlende Standard-QMI-Aktivierung erklärt den Totalausfall nicht |
| Sprache vorbereiten | USB/IMS/VoLTE lesen, gerätegebunden sichern, Leerlauf prüfen, nur nötige Bits ändern, zurücklesen, CFUN und erneute Geräteprüfung | Wesentliche Schritte in `call_mode_lifecycle.rs` vorhanden | Wiederanlauf muss diese vorhandene Funktion wieder erreichbar machen |
| ADB | AT ist nach Neustart früher bereit als adbd; 35 Sekunden Wartefenster und ggf. erneute Autorisierung | Entsprechende Warte- und Bindungslogik vorhanden | Nicht durch sofortiges ADB-Probing nach erstem AT-Erfolg ersetzen |
| Laufzeitdateien | Fehlende/beschädigte Dateien führen zu needs_download; Grundverbindung bleibt unabhängig | Nicht erklärte Laufzeitreste können die gesamte Modulzulassung sperren | Downloadzustand von tatsächlicher physischer Wiederherstellung unterscheiden |
| Download-Endpunkt | Download wird unmittelbar von Deployment und Audio-Routenselbsttest gefolgt | Ebenfalls nicht ausschließlich eine Dateifunktion | Eine Download-Herkunft darf keine Berechtigung nach Gerätezugriffen liefern |
| Backup | Normales dauerhaftes Datenobjekt; Restore prüft IMEI, USB-Komposition und Sprachwerte | Ungeklärte Backups gehören zur historischen Evidenzprüfung | Existenz eines Backups allein beweist weder aktiven Auftrag noch abgeschlossene Wiederherstellung |
| Beenden | 2.67 wartet gemeinsam bis 65 Sekunden auf Kommunikation und prüft RF-Restore; DJ besitzt kürzere separate Wartezeiten | Originale Owner, Ressourcenfreigaben und dauerhafte Recovery-Nachweise werden strenger gebunden | Alte pauschale Zeitlimits nicht als erfolgreiches Aufräumen übernehmen |

Wichtige 2.67-Stellen: `main.go:310-414,906-970,3604-3648`,
`call_mode.go:485-639,715-1119`, `call_mode_backup.go:531-685`,
`communication_shutdown.go:11-106`.
Die Diagnose erfolgt gegen den eingefrorenen Rust-Ausgangsstand; nachfolgende
Zeilennummern können sich durch Korrekturen verschieben.

## Nachgewiesene Fehler und offene Zuordnung

1. **Totale Sperre vor dem Modulstart.** In `main.rs:635-773,944-984,1051-1082`
   werden historische Zustände zuerst geprüft. Bei Quarantäne entstehen weder
   gewöhnliche Modul-/Call-Mode-Dienste noch die AT/SMS/QMI-Produzenten.
   Die untersuchte Diagnose passt zu diesem Ablauf: sofortige Modul-HTTP-409,
   ohne zugehörige Modulstart-Ereignisse. Das ist kein Beleg für einen USB-Defekt.

2. **Kein allgemeiner Wiederherstellungspfad für Altzustände.**
   `startup_recovery/reconcile.rs` und `execution.rs` benötigen ein typisiertes
   aktives Journal und eine passende Originalbindung. Ein normal benutztes
   2.67-Datenverzeichnis besitzt diese neue Journalstruktur nicht automatisch.
   Die normalen Retry-/Restore-Endpunkte werden gleichzeitig von der Gate
   gesperrt. Eine generelle Entsperrung oder das Löschen von Evidenz ist keine
   zulässige Reparatur.

3. **Download kann einen Wiederanlauf blockieren.** Der ursprüngliche Downloader
   legt Verzeichnisse vor dem Abschluss aller Dateien an. Die Pristine-Prüfung
   akzeptiert nur das vollständige Manifest; die Migration alter leerer Locks
   fordert dagegen die Abwesenheit des Runtime-Verzeichnisses. Damit fehlen
   Übergänge für einen fehlgeschlagenen oder rein dateiseitig abgeschlossenen
   kontrollierten Download. Nach Deployment gelten andere, physische Pflichten.

4. **Die entscheidende Ursache fehlt im Supportexport.** Die Runtime-Antwort
   kennt `reason_codes`, der bisherige Export enthält jedoch nur generische
   HTTP-Ergebnisse. Tausende erfolgreiche API-Protokolle ersetzen diese
   fehlende Information nicht. Der konkrete historische Grund des gemeldeten
   Totalausfalls bleibt daher unbewiesen.

5. **Die UI verweist auf möglicherweise unsichtbare Information.**
   Der Ursachenbereich startet mit einem leeren bedingten View; eine
   fehlgeschlagene Abfrage verschwindet ohne Fehlermeldung. Der separate
   Modultext verweist trotzdem auf darüberstehende Gründe. Gründe müssen am
   sichtbaren Sperrhinweis erscheinen; bei Lesefehlern muss der Zustand als
   unbekannt erkennbar bleiben.

Der Übergang 3.54 zu 3.55 änderte die Startup-Gate nicht. Ein erst beim
Neustart sichtbarer Zustand darf nicht als neu eingeführte Regel von 3.55
ausgegeben werden.

## Korrekturumfang und Grenzen

- Begrenzten aktuellen Recovery-Snapshot unabhängig von Logrotation exportieren:
  geschlossene Ursachencodes, Metadaten-/Reconciliation-/Native-Prüfergebnis
  und dreistufige Cleanup-Beobachtung. Keine Pfade, Identitäten oder Rohbefehle.
  Supportexport darf selbst keine Geräteprüfung oder Freigabe auslösen.
- Den Ursachenbereich zuverlässig laden und direkt beim Modulhinweis zeigen.
  Veraltete/fehlgeschlagene Abfragen dürfen weder Freigabe noch aktuelle
  Quarantäne eines anderen Backend-Prozesses behaupten.
- Kontrollierte reine Download-Dateiübergänge dauerhaft und eng an Ursprung,
  Root, Locks und Manifest binden. Unbekannte Dateien, manipulierte Metadaten,
  widerrufene Herkunft, aktive Journale und physische Pflichten bleiben gesperrt.
- Für echte alte Gerätepflichten nur vorhandene begrenzte Wiederherstellungs-
  Owner mit Originalbindung, realem Readback, Join, Freigabe und dauerhafter
  Veröffentlichung erweitern. Ein Dateinachweis ersetzt diese Schritte nicht.

Die Download-Korrektur gilt nur für eine nachgewiesene reine lokale Herkunft:
Das angeschlossene Modul muss bereits passend konfiguriert sein, sodass der
Status direkt `needs_download` erreicht. Ein vorheriges Enable, Deployment,
Selbsttest oder eine bereits vorhandene Gerätesicherung widerruft beziehungsweise
verhindert diese Berechtigung. Damit ist nicht die gesamte Kette
Enable → Download → Neustart abgenommen.

Diese ersten Korrekturen lösen nicht automatisch jeden unbekannten
2.67-Altzustand. Insbesondere darf ein erfolgreicher reiner Download nicht
als erfolgreicher physischer Download-/Deployment-Endpunkt ausgegeben werden.

## Erforderliche Abnahme

1. Beide echten Referenzen lesen und Abweichungen dokumentieren.
2. Ursache im Supportpaket auch ohne Modulstart, bei Logrotation,
   Queue-Ausfall und fehlgeschlagener Metadatenprüfung erhalten.
3. UI-Erstladen, Fehler, Backendwechsel, verspätete Antwort und Cancellation
   sowie die geschlossene Export-Whitelist testen.
4. Gleiche Datenwurzel über zwei echte Lifetime-Generationen prüfen:
   Erstdownload, Teilfehler, Wiederholung, alte leere Locks und vollständiger
   Download; negative Fälle für Journal, Widerruf, aktive Owner,
   Dateimanipulation und Symlinks.
5. Drei Quellprüfungen und die gemeinsamen GitHub-Gates am abschließenden SHA.
   Ein neuer sauberer temporärer Datenordner allein ist keine Upgrade-Abnahme.

Alle Ausführung und Kompilierung bleiben auf GitHub-Runnern. Quellabdeckung,
Softwareprüfungen und Geräteabnahme werden getrennt berichtet. Kein neuer
Release, kein DMG und keine Aussage über reale Netz-/Sprachqualität allein
aufgrund statischer Analyse oder Offline-Fixtures.

## Einheitliches Laufzeitprotokoll

Die getrennte Ansicht für Operationsereignisse wird in das Laufzeitprotokoll
überführt; die SMS-Speicherkarte entfällt auf der Diagnoseseite. Die zugrunde
liegenden zuverlässigen Zustands- und Operationsdaten bleiben erhalten.

Die Live-Ansicht verwendet einen begrenzten Speicherpuffer des aktuellen
Backend-Prozesses statt wiederholter Lesevorgänge über die rotierenden Dateien.
Sie zeigt Backend-Zustand, die zuletzt beobachtete Modulquarantäne mit ihren
Ursachencodes und die gemeinsamen technischen Ereignisse. Pausierte Anzeige,
Prozesswechsel, nicht erreichbarer Backend-Prozess, verworfene Einträge und
gekürzte Zeitfenster müssen erkennbar bleiben. Eine neue Snapshot-Uhrzeit ist
kein Nachweis einer neuen physischen Geräteprüfung.

Der separate Export bleibt verfügbar und enthält das begrenzte persistente
Zeitfenster. Die Live-Ansicht darf weder Geräteoperationen auslösen noch
SMS-Inhalte, Identitäten, Zugangsdaten oder ungeprüftes stderr übernehmen.

## Aktuelle Altzustandsprüfung (noch ohne Geräteabnahme)

Für begrenzte alte Sicherungs- und Cachebestände ohne aktive physische
Wiederherstellungspflicht gibt es einen separaten Rust-Startpfad. Er hält die
ursprüngliche Datenwurzel und beide tatsächlichen Locks, liest die bekannte
Metadatenstruktur und prüft anschließend dieselbe physische USB-Verbindung.
Alle unterstützten AT-Schnittstellen bleiben exklusiv beansprucht. Ein Wechsel
zwischen Kandidaten derselben Verbindung ist nur nach vollständigem AT-Schreiben
mit leerem Antwortfenster oder einer vollständigen ERROR-Antwort zulässig.
Ein ungewisser Schreibvorgang erlaubt keinen weiteren Kandidaten.

Die Prüfung liest Identität, vollständige USB-Konfiguration, IMS/VoLTE,
CFUN und den vollständigen Gesprächsstatus. Hinzu kommen begrenzte feste
ADB-Leseabfragen auf derselben Verbindung zur Bestätigung des Bootvorgangs
und eines ruhenden Sprachdienstes. Es werden weder Geräteeinstellungen
zurückgeschrieben noch fremde Prozesse beendet oder alte Dateien entfernt.
Alle Schritte einschließlich Aktivierung und erster Wiederöffnung teilen sich
eine absolute Frist von 30 Sekunden und dasselbe Abbruchsignal.
Blockierende Dateiarbeit läuft außerhalb des asynchronen Haupttasks.

Das Ergebnis ist eine kurzlebige aktuelle Beobachtung, kein nachträglicher
Beleg für historische Wiederherstellung. Der erste normale Besitzer muss
dieselbe Geräteverbindung und die bereits gewählte AT-Schnittstelle erneut
bestätigen; serielle Ausweichsuche und eine unabhängige automatische
QMI-Geräteauswahl sind in diesem Übergang deaktiviert. CFUN 0, 1 und 4 werden
als tatsächlich beobachtete Zustände erhalten, bis eine ausdrückliche
erfolgreiche Benutzeroperation die normale Präferenzverwaltung übernimmt.

Aktive oder archivierte Restore-Journale, ausstehende alte Ausführung,
verwaltete Sprachsteuerung und unbekannte physische Transaktionen bleiben
gesperrt. Die neue Prüfung akzeptiert derzeit nur Sicherungen einer einzigen
IMEI und benötigt eine überprüfbare ADB-Verbindung. Fehlendes ADB, ein
nicht bestätigter Sprachdienst, ein Gerätewechsel, ein abgelaufener Zeitrahmen
oder eine unbestätigte Ressourcenfreigabe bleiben konkrete Fehler.
Diese Einschränkungen dürfen nicht als erfolgreiche Modulabnahme ausgegeben
werden. Die vorhandenen Nutzerdaten wurden nicht gelöscht oder verändert.

Der geschlossene Status `startup.legacy_current` wird unabhängig von den
begrenzten Ereignisfenstern gespeichert. Damit bleibt auch nach Rotation oder
vollem Ereignispuffer erkennbar, welcher aktuelle Prüfschritt nicht bestätigt
wurde. Alte Diagnosepakete ohne dieses Feld bleiben lesbar. Unbestätigte
Ressourcenfreigabe überschreibt keine frühere Fehlermeldung mit einem späteren
Erfolgsstatus; technische Identitäten, Rohantworten und Geheimnisse bleiben
außerhalb der Anzeige und des Exports.

## Live-Ansicht und offene Validierung

Das Laufzeitprotokoll zeigt standardmäßig alle zulässigen technischen Einträge;
Warnungen und Fehler lassen sich gesondert filtern. Die letzte aktuelle
Prüfungsursache steht vor den historischen Ursachencodes. Die Liste wird alle
zwei Sekunden ohne parallele Folgeabfragen aktualisiert, solange die Ansicht
sichtbar ist. Ein ausgeschaltetes automatisches Scrollen beendet die Erfassung
nicht; das Verlassen der Ansicht beendet deren Abfragen.

Die GUI beobachtet denselben Backend-Prozess bis zu 210 Sekunden während
begrenzter Start- und Wiederherstellungsarbeit. Ein früher tatsächlicher
Prozessausgang wird sofort gemeldet. Ein lebender, danach noch nicht bereiter
Prozess bleibt erhalten; aus einer Zeitüberschreitung entsteht weder eine neue
Prozessinstanz noch eine erfundene Abschlussbestätigung.

Die Software-Fixtures decken virtuelle Startzeiten, veraltete Antworten,
geschlossene Diagnosefelder, überfüllte Ereignisfenster, echte Dateileases,
normalen Besitzerstart, Sicherungstransaktionen, bestätigten Shutdown und eine
neue Lifetime ab. Ein isolierter 900×650-Hosting-Test prüft die Log-Unteransicht;
er beweist nicht das Layout des gesamten App-Fensters. Der gezielte [Cloud-Lauf 40515525655](https://github.com/Proletariuss/Switch/actions/runs/40515525655)
hat die sichtbare Live-Unteransicht, die begrenzte Startbeobachtung und die
geschlossenen Supportexporte erfolgreich geprüft. Ein später ergänzter Test
für Clientwarnungen bei Backend-Ausfall sowie die gemeinsamen finalen Gates
stehen noch aus. Fehlgeschlagene Testläufe bleiben als solche sichtbar.


## Umfang der Ereigniserfassung und bewusste Abgrenzung

Die Ansicht vereinigt die bereits vorhandenen Client-, Backend-, Modul- und
Operationsereignisse. Sie ist kein vollständiges Audit aller internen Schritte.
Insbesondere beendet `call_mode_runtime_download.rs::run_native` einen eigenen
Downloadworker; `call_mode_lifecycle.rs::download` kann danach die separate
Gerätevorbereitung beginnen. Nicht jede dieser asynchronen Phasen besitzt
bereits ein eigenes Logereignis. HTTP 202 bedeutet deshalb nur Annahme;
vollständige Cachedateien beweisen weder die Bereitstellung im Gerät noch
einen erfolgreichen Audiotest.

Die spätere gezielte Ergänzung kann zwei getrennte geschlossene Operationen
für Cachedownload und Gerätevorbereitung verwenden. Deren Abschluss müsste
am tatsächlichen Worker und an der bestätigten Freigabe des bestehenden
Besitzers gemessen werden. Die typisierten Vorbereitungs- und Freigabefehler
lassen sich projizieren; Downloads benötigen eine feste Klassifikation am
Fehlerursprung. Freie Fehlermeldungen werden dafür weder durchsucht noch als
URL, Dateipfad, Identität oder Rohkommunikation gespeichert.

Diese zusätzliche Instrumentierung wird nicht mit der aktuellen Umstellung
der Anzeige verbunden: Sie verändert weitere Workergrenzen und erfordert
eigene Negativtests. Die aktuelle Änderung erhält deren Verhalten und führt
die vorhandenen Ereignisse zusammen. Bei einem Backend-Lesefehler bleiben
auch die aktuellen Clientwarnungen über verworfene oder nicht gespeicherte
Einträge sichtbar. Abbruch und Fristablauf während der Metadatenprüfung
werden getrennt von tatsächlich veränderten Daten ausgewiesen; unbestätigte
Ressourcenfreigabe behält Vorrang.
