## Änderung

Welches konkrete Problem wird behoben und wie verhält sich die Software danach?
Beschreibe den kleinsten notwendigen Eingriff und die betroffenen Grenzen.

## Nachweise

- [ ] Der PR ist bereit zur Prüfung; Draft gilt nicht als Freigabe.
- [ ] `Review result` ist für den aktuellen PR-Stand erfolgreich.
- [ ] `RustSec PR (rust)` und `RustSec PR (scripts/acceptance/daemon-capsule-fixture)` sind erfolgreich.
- [ ] `Review policy regression fixtures` ist erfolgreich.
- [ ] Die ausgewählten gemeinsamen Rust-/Swift-Prüfungen und gegebenenfalls der App-Build sind erfolgreich.
- [ ] Die Tests liefen auf GitHub; lokale Nachweise wurden nicht als Cloud-PASS übernommen.
- [ ] Keine Zugangsdaten, Teilnehmerkennungen, Laufzeitdatenbanken, privaten Logs, Caches oder nicht angeforderten DMGs sind enthalten.
- [ ] Rust-Backend-Zuständigkeit, gesperrte Abhängigkeiten und Versionsverträge bleiben erhalten.

Läufe und getesteten Commit verlinken. Nicht ausgeführte oder noch offene Prüfungen
ausdrücklich nennen.

## Abnahmegrenze

Softwaretests und Offline-Fixtures belegen keine reale Karte, Mobilfunkverbindung,
SMS-Zustellung, Audiofunktion, macOS-27-Laufzeit oder Langzeitstabilität.
Welche dieser Eigenschaften bleiben ungeprüft?

## Risiko und Rücknahme

Betroffene Lebenszyklus-/Protokollgrenze, erwartetes Fehlerverhalten und eine
gezielte Rücknahme nennen. Bereits veröffentlichte Tags und Anlagen bleiben
unverändert.

> Nach erfolgreicher Prüfung wird manuell zusammengeführt. Abhängigkeits- und
> Wartungs-PRs werden nicht automatisch gemerged. CODEOWNERS ist keine zusätzliche
> unabhängige Freigabe; eine erforderliche menschliche Bestätigung muss tatsächlich
> vorliegen.
