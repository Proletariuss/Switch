# Sicherheit und verantwortungsvolle Nutzung

## Betrugsprävention und Verbot von Missbrauch

Switch ist ausschließlich für rechtmäßige, autorisierte und von der Lizenz gedeckte nichtkommerzielle Anwendungen vorgesehen. Betrug, Phishing, Spam, Identitätsmissbrauch, unbefugte Überwachung sowie das unbefugte Abfangen, Sammeln oder Weiterleiten von SMS und Verifizierungscodes sind untersagt. Gesetzliche Vorgaben, erforderliche Identitätsprüfungen und Regeln der Netzbetreiber dürfen nicht umgangen werden.

Diese Verhaltensregeln gelten für alle Nutzer. Sie ersetzen weder geltendes Recht noch die [Lizenzbedingungen](LICENSE) und stellen keine technische Zugangssperre dar. Die ausführliche Projektrichtlinie steht am Anfang der [README](README.md).

## Sicherheitsprobleme vertraulich melden

**Dieses Repository ist öffentlich. Issues, Kommentare und Anhänge sind öffentlich sichtbar. Dort niemals Zugangsdaten, private Nachrichten oder Details einer noch nicht behobenen Schwachstelle veröffentlichen.**

Wenn unter „Security → Advisories“ die Schaltfläche „Report a vulnerability“ verfügbar ist, kann eine Schwachstelle über die private GitHub-Meldefunktion gemeldet werden. Fehlt diese Möglichkeit, zunächst nur um einen vertraulichen Kontaktweg bitten, ohne Schwachstellendetails, personenbezogene Daten oder Geheimnisse anzugeben.

Eine vertrauliche Meldung sollte betroffene Version, Voraussetzungen, erwartetes und tatsächliches Verhalten sowie bereinigte Reproduktionsschritte enthalten. Tests ausschließlich an eigenen oder ausdrücklich freigegebenen Systemen durchführen. Keine fremden Nachrichten abrufen und keine unautorisierten Anrufe oder SMS als Nachweis auslösen.

Für Missbrauch eines Anschlusses oder Kontos den zuständigen Netzbetreiber beziehungsweise Dienstanbieter kontaktieren. Das Projekt ist kein Notruf- oder Strafverfolgungsdienst.

## Vor jeder Veröffentlichung entfernen

- Telegram-Bot-Token, Bark-Schlüssel, Proxy-Zugangsdaten und andere Zugriffstoken;
- IMSI, ICCID, IMEI, EID, vollständige Rufnummern und eSIM-Aktivierungscodes;
- SMS-Inhalte, Verifizierungscodes, SIP/IMS-Sitzungskennungen und wiederverwendbare Authentifizierungsdaten;
- Schlüsselbundinhalte, private Anwendungsdaten, private Schlüssel und Umgebungsdateien mit persönlichen Pfaden.

Falls ein Geheimnis bereits in einem Commit, Protokoll oder Anhang veröffentlicht wurde, muss es beim jeweiligen Dienst widerrufen und ersetzt werden. Das Entfernen des Klartexts aus Git macht eine bereits erfolgte Offenlegung nicht rückgängig.

## Geräteoperationen

Schreibzugriffe auf Karten, Profilwechsel, Änderungen der USB-Konfiguration, die Wiederherstellung einer Modulkonfiguration sowie das Aktivieren von Telefonie oder Mobilfunkdaten können laufende Verbindungen unterbrechen. Vor solchen Eingriffen Geräteidentität prüfen, verfügbare Sicherungen erstellen und konkurrierende Karten-, SMS- oder Anrufvorgänge beenden.

## Grenzen der Zusicherungen

Softwaretests ersetzen keine Abnahme mit realen Geräten, Netzbetreibern, Proxy-Netzen oder unter Dauerlast. Ungeprüfte Funktionen dürfen nicht als garantierte Notfall- oder kritische Kommunikationsinfrastruktur betrachtet werden. Ein erfolgreicher Build bestätigt weder vollständige Fehlerfreiheit noch die Verhinderung von Betrug oder Missbrauch.
