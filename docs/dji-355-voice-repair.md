# DJI: Datenverbindung und Sprachzustand im Kandidaten 3.55

## Befund und Grenze

Ausgangspunkt ist 3.54, `5fe4528ef835f81f9a86ced7082fbe3cbc28a91e`.
Die bereitgestellte neue Diagnose enthält Version 3.54/354 mit dem passenden
Rust-Quelldigest. Im aktuellen Lauf wurden USB-Bootstrap, eSIM-Warmup, tatsächliche
Reservierungsfreigabe und SMS-Bereitschaft erfolgreich protokolliert. Die zuvor
korrigierte Freigabe ist somit in dieser Sitzung sichtbar; die neue Störung darf
nicht pauschal derselben Ursache zugeschrieben werden.

Danach sind sieben abgelehnte Laufzeit-Downloads und drei HTTP-200-Antworten
auf Auflegen sichtbar. Vier SMS-Aktualisierungen scheiterten, davon zwei vor und
zwei nach Abschluss des Warmups. HTTP 200 bestätigt nicht den anhaltenden
Sprachzustand. Rohdiagnosen, Nummern und Identitäten werden nicht veröffentlicht.

Der Screenshot zeigt als Rufnummer `,128`. Der bisherige Parser teilte CLCC
nur sechsmal, verschluckte damit den Adresstyp in die Rufnummer und berücksichtigte
den Modus nicht. Die offizielle Quectel-Datenzeile
`+CLCC: 1,0,0,1,0,"",128` wurde dadurch als aktives Telefongespräch projiziert.
Ein solcher falscher Call-Eigentümer kann weitere SIM-Leseoperationen und
Sprachbereitstellung sperren. Die Diagnose speichert absichtlich keine Rohantworten;
welche konkrete Zeile das angeschlossene Gerät geliefert hat, ist damit nicht bewiesen.

Referenz: [Quectel EC2x/EG9x/EG2x-G/EM05 AT Commands Manual V2.0](https://quectel.com/content/uploads/2024/05/Quectel_EC2xEG9xEG2x-GEM05_Series_AT_Commands_Manual_V2.0.pdf),
gedruckte Seiten 114 und 127–128. Die frühere Switch-2.67-Quelle und
DJOneHubNative bleiben Verhaltensreferenzen; eine pauschale Regel
„keine Nummer bedeutet kein Anruf“ wird nicht übernommen.

## Änderungen in Rust

- Ein begrenzter, gemeinsamer CLCC-Parser trennt Status, Modus, Rufnummer,
  Adresstyp und optionale Anruferkennung. Nur der ausdrücklich bekannte
  Datenmodus 1 wird aus der Sprachbelegung ausgeschlossen.
- Anzeige, strenger Sprach-Leerlaufnachweis und Call-Mode-Zulassung verwenden
  dasselbe Ergebnis. Mode-0-Anrufe mit unbekannter Nummer, gehaltene Anrufe,
  gemischte Listen, unbekannte Modi, fehlerhafte oder unvollständige Antworten
  bleiben geschützt. Späte RING-/CONNECT-Meldungen dürfen keinen Leerlauf beweisen.
- CLCC-Lesezugriffe verlangen bereits im nativen USB-Transport eine vollständig
  abgeschlossene Antwortzeile. Ein Fristablauf mit den Buchstaben OK ohne
  abschließenden Zeilenumbruch darf nach Textnormalisierung keinen Leerlauf
  bestätigen. Guards vor dem tatsächlichen Schreiben und die Partial-Kompatibilität
  anderer AT-Befehle bleiben erhalten.
- Ein akzeptiertes ATH wird einmal unter derselben Reservierung durch CLCC
  überprüft. Solange das Ende nicht bestätigt ist, bleibt die Schutzabsicht auch
  bei vorher unbekanntem Zustand erhalten. Es gibt keine automatische Wiederholung
  des Schreibbefehls. Neuere Klingel-, Terminal- und Verbindungsereignisse schlagen
  die ältere Bestätigung; die Audioroute eines neueren Anrufs wird nicht beendet.
- Ein noch nicht angelegter Sprach-Laufzeitordner wird als „noch nicht heruntergeladen“
  ausgewiesen. Ungültige Verzeichnisse und symbolische Links bleiben Fehler.
  Statusabfragen erstellen keine Verzeichnisse; der vorhandene Downloader verwendet
  weiter verzeichnisgebundene, nicht folgenden Zugriff und 0700-Berechtigungen.
  Download und anschließende Modulbereitstellung behalten ihre Reservierung.

## Diagnose

Call-Abfragen und die tatsächliche Übernahme/Freigabe von Call-Reservierungen
erhalten begrenzte Ereignisse mit geschlossenem Vokabular. Erfolgreiche Abfragen
melden Sprach-Leerlauf, vorhandene Sprache oder unbestätigten Zustand aus dem
bereits berechneten Snapshot. Besitzerfakten bei einem Konflikt werden unter
dem zuständigen Lock kopiert; geschrieben wird außerhalb des Locks.
Es werden weder Rufnummern noch Rohantworten, Pfade oder Tokens protokolliert.
Die neue Stufe CallQueryPhysical belegt die Annahme durch den Actor; allein
belegt sie keine tatsächlich gesendeten AT-Bytes. Swift exportiert die
entsprechenden erlaubten Felder, ohne seine Datenschutzprüfung zu lockern.

## Prüfung

Die Komponenten werden getrennt und gegenseitig geprüft. Beim Hangup-Review wurde
zusätzlich die Schutzlücke bei unbekanntem Anfangszustand entdeckt und vor der
integrierten Abnahme korrigiert. Regressionen verwenden den tatsächlichen Parser,
CLCC-Felder einschließlich Adresstyp, konkurrierende Ereignisse und unveränderte
Produktions-Zulassung. Reine Anzeige-Fixtures ersetzen keine Owner-Prüfung. Zusätzliche Tests führen
standardgemäße, fragmentierte Antworten durch NativeTransport und verwenden
tatsächliche USB-Owner, Radio-Service, NativeDriver und HTTP-Handler mit
synthetischem Transport. Sie prüfen die ursprüngliche Token-/Sequenzfreigabe,
anschließende Call-Mode-Zulassung, unbekannte Nummern und nicht erneut gesendete
Schreibbefehle. Das ersetzt weiterhin kein reales Gerät.

Die abschließenden Ergebnisse, genaue Revision und das Kandidatenartefakt stehen
im zugehörigen PR und seinen GitHub-Actions-Läufen. Erst erfolgreiche Gates des
endgültigen Quellstands schließen die Softwareabnahme. Vorherige 3.54-Ergebnisse
werden nicht als Nachweis für 3.55 verwendet.

Alle Quelländerungen, Formatierung, Tests und Builds erfolgen über GitHub bzw.
GitHub-gehostete Runner. Diese Änderung liefert eine Kandidaten-App, keinen DMG
und keinen neuen Release-Tag. Reale DJI-/SIM-Funktion, Betreiberregistrierung,
Audio, macOS-27-Ausführung und Langzeitstabilität bleiben ohne neue
Gerätenachweise unbestätigt.
