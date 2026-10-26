# PR-Prüfung und manuelle Freigabe

Quelländerungen erfolgen über GitHub-APIs. Formatierung, Tests, Reparaturjobs und
Builds laufen ausschließlich auf GitHub-gehosteten Runnern. Der lokale Mac dient
nur als lesende Quelle und API-Steuerung. Ein Prüfauftrag erlaubt nicht automatisch
die Korrektur zusätzlich entdeckter Geschäftslogik; dafür gilt der bestätigte
Auftragsumfang. Es wird keine Codex-Anmeldung oder automatische Codeprüfung
eingerichtet.

## Ablauf

1. Einen Arbeitsbranch vom aktuellen `main` erstellen und die Änderung als PR
   vorlegen. Bestehende erfolgreiche Software-Prüfungen sind kein Ersatz für
   Nachweise des neuen Quellstands.
2. Draft führt portable Prüfungen, beide RustSec-Prüfungen und die Richtlinien-
   Fixtures aus. `Review result` bleibt ausdrücklich fehlgeschlagen, solange
   der PR Draft ist.
3. „Ready for review“ löst die Prüfung erneut aus. Native Rust-/Swift-Prüfungen
   und der App-Build richten sich nach den geänderten Pfaden. Ein erforderliches,
   aber übersprungenes oder gescheitertes Gate darf keine Freigabe ergeben.
   Die Rückkehr zu Draft löst ebenfalls einen neuen Lauf aus.
4. Änderungen unter `.github/workflows/` erhalten native und App-Prüfungen.
   AGENTS, CODEOWNERS, die PR-Vorlage und sonstige Dokumentation bleiben bei
   portablen Prüfungen, RustSec und Richtlinien-Fixtures.
5. Den aktuellen Diff, offene Diskussionen und die erfolgreichen Pflichtprüfungen
   prüfen, anschließend manuell mergen. Es gibt kein Auto-Merge und keinen
   Ersatznachweis durch einen Wartungs- oder Entwicklungslauf.

## Exakte Pflichtprüfungen

| PR-Prüfung | Quelle |
| --- | --- |
| `Review result` | GitHub Actions, App-ID `15368` |
| `RustSec PR (rust)` | GitHub Actions, App-ID `15368` |
| `RustSec PR (scripts/acceptance/daemon-capsule-fixture)` | GitHub Actions, App-ID `15368` |
| `Review policy regression fixtures` | GitHub Actions, App-ID `15368` |

Die geplante Branch-Regel verlangt diese vier Prüfungen, Aktualität gegenüber
`main` (strict), aufgelöste Diskussionen und Einhaltung auch durch Administratoren.
Force-Push und Löschen von `main` sind verboten. Die Regel wird erst nach einem
erfolgreichen integrierten PR-Lauf separat aktiviert und über die API bestätigt;
diese Datei allein aktiviert keinen Schutz.

Die vorgesehene Zahl erforderlicher Genehmigungen ist `0`; damit wird keine
unabhängige zweite Person erzwungen. CODEOWNERS benennt `@Proletariuss` als
zuständig. Zusätzliche Codeowner- oder Last-Push-Genehmigungen werden nicht
stillschweigend vorausgesetzt.

## Getrennte Kontexte

Manuelle und geplante Qualitätsläufe heißen `Maintenance result`.
Inaktive RustSec-PR-Jobs außerhalb eines PRs verwenden `RustSec PR inactive (…)`.
Push- und manuelle Richtlinienprüfungen heißen
`Review policy development fixtures`. Diese Namen ersetzen keine Pflichtprüfung
des PRs. Die gemeinsame Software-Prüfung bleibt
`.github/workflows/software-gates.yml`; alte monolithische Workflow-Dateien
dürfen sie nicht überschreiben.

PR-Jobs verwenden ausschließlich lesende Token, fixierte Action-SHAs und
Checkouts ohne gespeicherte Zugangsdaten. Die vorhandenen manuellen bzw.
geplanten RustSec-Jobs behalten nur ihre ausdrücklich benötigten Check-/Issue-
Rechte; diese gelten nicht für PR-Quellcode.

## Veröffentlichung und historische Vorbereitung

Reguläre Veröffentlichungen werden ausdrücklich aus `main` mit dessen genauem
Commit gestartet. Es gelten die unabhängigen Branch-Prüfungen im Identity-Job,
Publish-Job und Publisher sowie der
[Veröffentlichungsvertrag](CLOUD_RELEASE_CONTRACT.md).

Die einmalige `prepare_reviewed`-Eingabe und ihr schreibender Workflow sind
entfernt. `scripts/ci/prepare-reviewed.py` und die frühere Autorisierung bleiben
historische Nachweise, kein unterstützter Einstieg. Historische Branches können
alte Workflow-Versionen enthalten; neue Regeln ändern diese alten Versionen
nicht rückwirkend. Sie dürfen nicht für neue Veröffentlichungen verwendet werden.

Das Software-Release [v3.52](https://github.com/Proletariuss/Switch/releases/tag/v3.52)
bleibt unverändert. GitHub-Softwaretests ersetzen weder Geräte-/Betreiberabnahme
noch reale macOS-27- oder Langzeitnachweise.
