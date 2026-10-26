<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/AppIcon-dark.png">
    <img alt="Switch" src="docs/AppIcon-light.png" width="128">
  </picture>
</p>

<h1 align="center">Switch</h1>

<p align="center">Mobilfunk und physische eSIM-Karten auf dem Apple-Silicon-Mac verwalten</p>

<p align="center">
  <a href="LICENSE"><img alt="Lizenz" src="https://img.shields.io/badge/Lizenz-PolyForm%20Noncommercial-orange"></a>
  <img alt="macOS" src="https://img.shields.io/badge/macOS-27.0%2B-black?logo=apple&logoColor=white">
  <img alt="Architektur" src="https://img.shields.io/badge/Architektur-Apple%20Silicon-blue">
  <img alt="Backend" src="https://img.shields.io/badge/Backend-Rust-black?logo=rust&logoColor=white">
  <img alt="Version" src="https://img.shields.io/badge/Version-3.51-2f81f7">
</p>

## Verbindliche Projektrichtlinie: Kein Betrug, kein Missbrauch

> [!WARNING]
> **Betrug, Phishing, Identitätsmissbrauch, Spam und unbefugte Überwachung sind ausdrücklich untersagt.**
> Switch ist ausschließlich für rechtmäßige, autorisierte und von der Lizenz gedeckte nichtkommerzielle Anwendungen vorgesehen. Verwenden Sie nur SIM-Karten, eSIM-Profile, Konten und Geräte, zu deren Nutzung Sie berechtigt sind.

- **Keine betrügerische Kommunikation:** keine Täuschung über Absender oder Identität, keine betrügerischen Anrufe, Nachrichten oder Zahlungsaufforderungen.
- **Kein Missbrauch von SMS oder Verifizierungscodes:** kein unbefugtes Abfangen, Sammeln oder Weiterleiten von SMS, Einmalpasswörtern oder Wiederherstellungscodes; keine Kontoübernahmen oder Umgehung von Kontoschutzmaßnahmen.
- **Keine unerwünschten Massensendungen:** keine Spam-Kampagnen, Belästigung oder rechtswidrige automatisierte Anruf- und Nachrichtenverteilung.
- **Keine Umgehung von Schutzvorgaben:** gesetzliche Anforderungen, erforderliche Identitätsprüfungen, Regeln der Netzbetreiber sowie Maßnahmen zur Betrugsprävention sind einzuhalten. Das Projekt unterstützt keine Umgehung von Zugriffs- oder Authentifizierungskontrollen.
- **Datenschutz und Einwilligung beachten:** Nachrichten nur mit entsprechender Berechtigung verarbeiten oder an Telegram, Bark beziehungsweise andere Dienste weiterleiten. Zugangsdaten und personenbezogene Informationen gehören nicht in öffentliche Issues, Protokolle oder Anhänge.
- **Bei Missbrauchsverdacht:** betroffene Weiterleitungen und Automatisierungen stoppen, gefährdete Zugangsdaten widerrufen und den zuständigen Dienstanbieter informieren. Sensible Belege nicht öffentlich veröffentlichen.

Diese Regeln gelten für alle Nutzer. Sie beschreiben die Verhaltensanforderungen des Projekts und ersetzen weder geltendes Recht noch die [Lizenzbedingungen](LICENSE) oder Rechte Dritter. Eine Dokumentation in deutscher Sprache ist keine technische Zugangsbeschränkung und keine Garantie gegen Missbrauch.

## Projekt und Veröffentlichungsstand

Switch bündelt USB/QMI-Module, PC/SC- und BLE-Kartenleser, physische eSIM-Profile, VoWiFi/IMS, SMS und Telefonie in einer macOS-Anwendung. Die veröffentlichte Version ist **3.51, Build 351, mit Rust-Backend 3.51.0**.

Das Repository ist seit dem **26. Oktober 2026 öffentlich**. Die Veröffentlichung des Quellcodes bestätigt keine vollständige Abnahme aller Geräte, Netzbetreiber oder Dauerbetriebsszenarien.

Der Tag `v3.51` bleibt dem eingefrorenen Quellstand des vorhandenen Installationspakets zugeordnet. `main` enthält seit dem 25. Oktober 2026 einen späteren Entwicklungsstand mit dem Status `source-not-final-G0B`. Weitere Änderungen werden über GitHub geprüft; sie machen das vorhandene 3.51-Paket nicht automatisch zu einem neuen Build. Einzelheiten zur Herkunft stehen in [SOURCE_PROVENANCE.md](SOURCE_PROVENANCE.md).

## Download

| Version | Plattform | Installationspaket | SHA-256 |
| --- | --- | --- | --- |
| 3.51 / Build 351 | Apple Silicon, macOS 27.0+ | [Switch-3.51-macOS27.dmg](https://github.com/Proletariuss/Switch/releases/download/v3.51/Switch-3.51-macOS27.dmg) | `616c276ef75da7965f3a95a9a754d52d4b1701f8802d29ba7cabc2dc0ba5cbde` |

[Veröffentlichung 3.51](https://github.com/Proletariuss/Switch/releases/tag/v3.51)

## Installation

1. Das DMG herunterladen und öffnen.
2. `Switch.app` in den Ordner „Programme“ ziehen.
3. Bei Bedarf die von macOS angeforderten Berechtigungen für Bluetooth, Mitteilungen und Mikrofon erteilen.
4. Das Paket ist ad hoc signiert und **nicht von Apple notarisiert**. Falls Gatekeeper den Start blockiert, zunächst Herkunft und Prüfsumme kontrollieren und die Freigabe anschließend unter „Systemeinstellungen → Datenschutz & Sicherheit“ vornehmen.

Bei einer Neuinstallation sind keine Telegram-Bot-, Bark- oder sonstigen Weiterleitungsschlüssel vorgegeben. Eine Aktualisierung über eine vorhandene Installation verwendet die lokalen Einstellungen und den Schlüsselbund weiter. Versionsübergreifende Migration und Rückkehr zu älteren Versionen sind nicht vollständig unabhängig abgenommen; wichtige Daten vor einer Aktualisierung sichern.

## Funktionen

| Bereich | Umfang |
| --- | --- |
| Modul und Netzwerk | Erkennung von DJI-Modulen der ersten Generation/QDC507, SIM- und Signalinformationen, QMI/AT-Verwaltung, Mobilfunk-Netzwerkschnittstelle, Netzwerkreihenfolge und Modul-VoWiFi |
| Kartenleser und eSIM | PC/SC- und BLE-Erkennung, Verbindungsaufbau, Auslesen von EID und Profilen, Profilverwaltung sowie vorhandene B0/B1/9eSIM-Integrationspfade |
| VoWiFi / IMS | Netzbetreiberprofile, eigene ePDG-Einstellungen, Proxy-Routing sowie IKE/IPsec- und IMS-Sitzungsverwaltung; vorhandene Konfigurationen belegen keine erfolgreiche Registrierung bei jedem Netzbetreiber |
| SMS-Zentrale | Nachrichtenarchiv für Module, Kartenleser und Mehrprofil-Bereitschaft, Sendestatus, Mitteilungen, Suche, Löschung und optionale Weiterleitung an Telegram Bot oder Bark |
| Mehrprofil-Bereitschaft | Unabhängige SMS-Sitzungen für mehrere Profile, Erneuerung, Wiederverbindung und Statusberichte; ausschließlich SMS, keine Sprache; Gleichzeitigkeit hängt von Karte und Netzbetreiber ab |
| Telefonie | Wählen, Annehmen, Auflegen, Anrufhinweise, Anrufprotokoll und Mac-Audioanbindung; bevorzugt EVS, alternativ AMR-WB |
| Diagnose | Netzwerk- und Gerätediagnose, AT-Konsole, strukturierte Zustände und Sicherheitsereignisse |

## Voraussetzungen und Geräte

- Apple-Silicon-Mac mit mindestens macOS 27.0.
- Für den Modulbetrieb: DJI-Modul der ersten Generation/QDC507; typische USB-Kennung `2ca3:4006`.
- Für den Kartenleserbetrieb: PC/SC-CCID-Leser, beispielsweise aus der ESTKme-Serie, oder ein kompatibles BLE-Gerät mit unterstützter B0/B1-Anbindung.
- Nutzbare physische SIM beziehungsweise eSIM-Profile, passende Netzbetreiberdienste und ein dafür geeigneter Netzwerkzugang.

Identische Modellbezeichnungen bedeuten nicht identische Firmware, USB-Konfiguration, Kartenauthentifizierung oder Netzbetreiberregeln. Vor Schreibzugriffen auf Karten, Profilwechseln oder der Wiederherstellung von Modulkonfigurationen die Geräteidentität prüfen und vorhandene Sicherungsmöglichkeiten nutzen.

## Architektur

```text
SwiftUI-Anwendung
    │  Lokaler Unix-Domain-Socket / typisierte API
    ▼
switchd-rs (Rust-Backend)
    ├── Geräteverwaltung für USB AT / QMI / PCSC / BLE
    ├── Zustandsautomaten für eSIM, VoWiFi, IMS, SMS und Telefonie
    ├── Lokale Speicherung, Wiederherstellung und Aufgabenwarteschlangen
    └── Eingebettetes libusb und erforderliche native EVS-Komponenten
```

Das Anwendungspaket benötigt kein Go-Backend. Swift übernimmt die Oberfläche und die Anbindung an macOS-Systemframeworks. Historische Migrations- und Prüfunterlagen liegen in `docs/`; sie sind kein Nachweis einer aktuellen Geräte- oder Netzbetreiberabnahme.

## Aus dem Quellcode bauen

Die vorgesehene Entwicklungsumgebung umfasst Apple Silicon, Xcode, XcodeGen, Rust 1.98.0, Python 3, `pkg-config` und arm64-`libusb`. Für eine Prüfung gegen das macOS-27-SDK ist eine entsprechende Xcode-Version erforderlich. Der erste Build benötigt gegebenenfalls Zugriff auf Paketquellen und die Rust-Registry.

```bash
brew install xcodegen pkg-config libusb
rustup toolchain install 1.98.0

scripts/build.sh                 # Erzeugt ../dist/Switch.app, kein DMG
scripts/launch.sh                # Startet mit isoliertem Laufzeitverzeichnis
scripts/quit.sh                  # Beendet Anwendung und zugehöriges Backend
scripts/preflight-source.sh      # Prüft Versionen und Quellcodeverträge
```

Die laufende Projektprüfung und Kompilierung erfolgen auf GitHub-gehosteten Runnern. Die Workflows prüfen Quellcodeverträge, Rust-Arbeitsbereiche und Softwaretests; empfindliche Tests für gemeinsam genutzte serielle Zustände, Abnahmekapseln und prozessweite Dateideskriptoren laufen isoliert. App-bezogene Änderungen werden zusätzlich kompiliert. Ein erfolgreicher Lauf auf `macos-26` belegt jedoch keinen Test unter macOS 27 und keinen automatisch vorhandenen macOS-27-SDK.

Die [GitHub-Automatisierungsrichtlinie](docs/GITHUB_AUTOMATION.md) beschreibt Prüfungen, Abhängigkeitsaktualisierungen und Berechtigungsgrenzen. `scripts/test-migration-gates.sh` enthält außerdem historische, hostbezogene Nachweise: Fehlen diese in einem normalen Klon, ist eine fehlgeschlagene Herkunftsprüfung nicht automatisch ein Fehler des Anwendungscodes oder des DMG.

Die Skripte setzen die im Projekt festgelegte Anordnung mit benachbarten `build/`- und `dist/`-Verzeichnissen voraus. Herkunft und ursprüngliche Veröffentlichungsschritte sind in [SOURCE_PROVENANCE.md](SOURCE_PROVENANCE.md) dokumentiert.

## Daten und Sicherheit

- Anwendungsdaten liegen standardmäßig unter `~/Library/Application Support/Switch`; vertrauliche Einstellungen bleiben lokal.
- Telegram Bot und Bark erhalten SMS-Inhalte nur nach ausdrücklicher Konfiguration und Aktivierung der Weiterleitung. Vor der Aktivierung die Berechtigung zur Weitergabe und den Empfänger prüfen.
- Die ursprüngliche Veröffentlichung schloss lokale Konfigurationsdateien aus und anonymisierte erkannte reale Teilnehmerkennungen. Prüfungen bekannter Geheimnismuster sind keine Garantie für die Abwesenheit aller sensiblen Daten.
- Vor einer Fehlermeldung IMSI, ICCID, IMEI, EID, vollständige Rufnummern, SMS-Inhalte, Aktivierungscodes, Proxy-Zugangsdaten und Sitzungskennungen entfernen.
- **Issues dieses öffentlichen Repositorys sind öffentlich sichtbar.** Keine unveröffentlichten Schwachstellendetails oder Zugangsdaten dort einstellen. Hinweise zur vertraulichen Meldung stehen in [SECURITY.md](SECURITY.md).

## Prüfstand und bekannte Grenzen

Für die veröffentlichten 3.51-Artefakte wurden folgende Prüfungen dokumentiert:

- Dateisystemprüfung des DMG;
- strenge Prüfung der Signaturstruktur von App und eingebetteten Komponenten;
- Abgleich von Version 3.51, Build 351, Backend 3.51.0, arm64 und Mindestversion macOS 27.0;
- Prüfung des Dateimanifests und bekannter Zugangsdatenmuster;
- leere und standardmäßig deaktivierte SMS-Weiterleitung bei einer Neuinstallation.

**Nicht als vollständig abgenommen gelten:** alle USB/PCSC/BLE-Gerätekombinationen, sämtliche VoWiFi/IMS-Netzbetreiberregistrierungen, reale SMS-Zustellung, ausgehende und eingehende Anrufe, Audioqualität, Wiederherstellung nach Netzverlust oder Ruhezustand sowie Langzeitstabilität. Maßgeblich sind getrennte Nachweise für die jeweilige Version, Karte, Hardware und Netzwerkumgebung.

Das DMG ist ad hoc signiert und nicht notarisiert; es ist kein von Apple notarisiertes Developer-ID-Paket. Switch ersetzt keinen Notrufdienst oder garantierte Kommunikationsverfügbarkeit.

## Integrität prüfen

```bash
shasum -a 256 Switch-3.51-macOS27.dmg
```

Erwartetes Ergebnis:

```text
616c276ef75da7965f3a95a9a754d52d4b1701f8802d29ba7cabc2dc0ba5cbde  Switch-3.51-macOS27.dmg
```

## Fehler melden

Für gewöhnliche Fehler ein [Issue](https://github.com/Proletariuss/Switch/issues) mit Version, Mac-Modell, macOS-Version, Gerätetyp und bereinigten Protokollen anlegen. Bei Netzbetreiberproblemen können MCC/MNC, Anschlussart und Netzwerkpfad helfen; vollständige Teilnehmerkennungen oder Schlüssel niemals veröffentlichen.

Für vertrauliche Sicherheitsmeldungen das Verfahren in [SECURITY.md](SECURITY.md) verwenden.

## Lizenz und Drittanbieter

Das Projekt steht unter [PolyForm Noncommercial 1.0.0](LICENSE). Es gelten die dort festgelegten nichtkommerziellen Nutzungsrechte. Drittanbieterkomponenten, erforderliche Hinweise und deren Lizenzen sind in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) sowie `licenses/` aufgeführt.

Die Verfügbarkeit von EVS-Code oder -Komponenten begründet keine zusätzliche Patentfreigabe. Die Herkunft einzelner Klingeltonressourcen ist weiterhin nicht vollständig dokumentiert. Die öffentliche Sichtbarkeit des Repositorys bestätigt daher weder eine vollständige Rechteklärung noch eine Erlaubnis zur kommerziellen Weiterverbreitung.
