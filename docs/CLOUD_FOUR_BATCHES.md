# Vier Ausführungsblöcke ausschließlich auf GitHub

Genehmigt am 26.10.2026. Ausgangscommit:
`dedaf50a94e1475883db80f2381a1b42ee15846b`.
Tree: `eab6f5d08ac2bd5224b5ab37b0fdde42ac8182f4`.

Quelländerungen verwenden GitHub-APIs. Tests, durch Jobs ausgeführte Korrekturen,
Kompilierung und Artefakterstellung laufen auf GitHub-gehosteten Runnern. Der
lokale Mac dient nur als lesende Quelle/API-Steuerung; dort laufen keine
Projekt-Reparaturen, Builds oder Tests.

## Festgelegte Abschlusskriterien

1. Gemeinsame Cloud-Gates: beide gesperrten Abhängigkeitsgraphen auf Linux,
   vollständige Rust- und Offline-Swift-Prüfungen, Upload-Rücklesetest und
   App-Artefakt für den exakten SHA.
2. Laufzeit: ursprüngliche Modulbindung bleibt beim Neustart erhalten;
   ein N-Standby-Installationsretry wird nicht verbraucht; ePDG-Aliase bewahren
   die Semantik der Rückmeldungen.
3. SMS: reine Archivwiederherstellung sendet nichts erneut; begrenzte dauerhafte
   Zuständigkeit, v1-Migration, sichere Endzustände und dauerhafte Swift-Ausgabe.
4. Drei Prüfrunden, Software-Gates des abschließenden Quellstands, App-Identität
   und leere Weiterleitungsdefaults, unveränderliche Prüfsummen, GitHub-Nachweise
   und benannte Abnahmegrenzen.

Jeder Block benötigt Nachweise für den abschließenden Quellstand. Bereits
veröffentlichte Tags/Releases einschließlich v3.51 und v3.52 bleiben unverändert.
Die Lieferung ist eine ad-hoc signierte CI-App, keine vollständige Produktabnahme
und kein neu erzeugtes DMG.

## Abhängigkeiten

PR #3 ändert ein verknüpftes Hauptmanifest ohne Lockfile; #5 benötigt eine
API-Migration; sha1 0.11 entfernt das verwendete IKE-compress-Feature.
Die kompilierte Hash-Familie bleibt für diese Stabilitätslieferung erhalten;
inkompatible Versionswechsel benötigen eine koordinierte Migration. Erneut
prüfen bis 25.11.2026. RustSec und Sicherheitsmeldungen bleiben ohne Advisory-
Ausnahmen aktiv; neue Meldungen benötigen eine neue Bewertung.

## Abnahmegrenze

Artefakte binden Commit, Tree/Quell-Digest, Run/Versuch, tatsächliche Toolchain
und Archiv-SHA. Historische lokale Nachweise ergeben kein Cloud-PASS.
Offline-Tests belegen keine physische Karte, Betreiber-/Provider-Verbindung,
reale Keychain, Benachrichtigung/Audio, macOS-27-Ausführung oder einen
24-/72-Stunden-Dauerlauf. Es sind keine Developer-ID-/Notary-Zugangsdaten
eingerichtet; die Runner verwenden macos-26.

## Nachgewiesener Softwarestand

[PR #6](https://github.com/Proletariuss/Switch/pull/6) ist zusammengeführt.
`main` wurde am 26.10.2026 ohne Force-Push auf den bereits veröffentlichten
Commit `d17eca3305bd2204cc895fc13af8ee0e91173f8b` fortgeschrieben.
Der [Release-Lauf 40314576638](https://github.com/Proletariuss/Switch/actions/runs/40314576638)
ist erfolgreich; [v3.52](https://github.com/Proletariuss/Switch/releases/tag/v3.52)
enthält App-Tarball, Originalmanifest, SHA256SUMS und Testnachweise.
Diese Nachweise gelten für d17eca3 und werden nicht auf spätere Änderungen
übertragen.

Die drei vorherigen statischen Prüfrunden sowie vorläufige Läufe bleiben
historische Zwischenstände. Der isolierte Swift-Stand 6b06ee8 bestand 29 Gruppen
in Lauf 40275765127. Lauf 40273933310 bestand 3684 daemon-Tests, scheiterte aber
an zwei später korrigierten Fixtures und erreichte nicht alle isolierten Gruppen;
er war keine abschließende Abnahme.

Neue Governance- und Funktionskorrekturen benötigen ihre eigenen integrierten
GitHub-Nachweise gemäß [PR-Regeln](PR_REVIEW_POLICY.md). Der erfolgreiche
Softwarestand vergibt weiterhin keine Geräte-, Betreiber- oder Produktabnahme.
