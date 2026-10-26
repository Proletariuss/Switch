# GitHub-Veröffentlichungsvertrag

Neue Softwarestände erhöhen VERSION, BUILD_NUMBER und die Rust-Workspace-Version
gemeinsam. Auf 3.51 / 351 / 3.51.0 folgt 3.52 / 352 / 3.52.0. Beide Cargo-Lockfiles
müssen die lokalen Workspace-Versionen enthalten; externe Abhängigkeiten bleiben
unverändert. Bestehende Tags und Releases werden weder umgehängt noch überschrieben.

Der manuelle Einstieg ist .github/workflows/release.yml mit candidate_sha und
ausschließlich dem ausgewählten Branch `main`. Dieser SHA muss genau dem
Workflow-Stand von main entsprechen. Identity- und Publish-Job prüfen den Branch;
der Publisher prüft unabhängig GITHUB_REF und head_branch der Run-API, bevor
Schreibzugriffe möglich sind. Die vollständigen gemeinsamen Software-Prüfungen
und der App-Build laufen für genau diesen Stand erneut.
Ein älterer erfolgreicher Lauf ist keine Freigabe für eine neue Versionsnummer.

Vor dem Packen prüft verify-app-identity.py das App-Plist und die eingebettete
Backend-Identität einschließlich Quell-Digest. Das ist keine Laufzeitabnahme.
Der SHA-haltige App-Tarball erhält Rechte und symbolische Links. Erst nach dem
tatsächlichen Upload, erneuten Download und erfolgreicher Byte-/Inventarprüfung
darf der getrennte publish-Job Schreibrechte verwenden. PR-Jobs erhalten diese
Berechtigung nicht. Ein Release-Einstieg erzeugt vor diesen Prüfungen keinen Tag.

Der Publisher prüft Quellen-SHA, Git-Tree, Version, Build, Run und ursprünglichen
Candidate-Versuch; er akzeptiert nur erfolgreiche Originalberichte. Ein reiner
Publish-Wiederholungsversuch verwendet den bereits geprüften Candidate desselben
Runs. Er erstellt einen Entwurf mit den vier ursprünglichen Audit-Anlagen:
App-Tarball, candidate-manifest.json, SHA256SUMS und test-reports.tar.gz.
Jede Release-Anlage wird über die API vollständig zurückgelesen und per SHA-256 geprüft.
Erst dann werden der neue Tag auf den geprüften Commit und das öffentliche Release
angelegt. Vorhandene Entwürfe sind nur mit identischem SHA, Manifest und identischen
Asset-Bytes fortsetzbar. Konflikte führen zum Abbruch; es gibt kein clobber/retag.

Release-Titel und Hinweise sind deutsch. Die App bleibt ad-hoc signiert und nicht
notarisiert. Hardware, Mobilfunkanbieter, reale macOS-27-Ausführung und
Langzeitstabilität bleiben ohne gesonderte Nachweise ungeprüft. Eine DMG-Verpackung
ändert diese Grenzen nicht.
Actions-Zwischenartefakte und Testberichte behalten sieben Tage Aufbewahrung;
veröffentlichte Release-Anlagen haben diese automatische Ablaufzeit nicht.
Ein Release verändert oder merged keinen Quellbranch.

Die Offline-Fixtures in scripts/ci/test-publish-release.py laufen ausschließlich
im GitHub-Portable-Gate. Sie prüfen Versions-/Quellbindung, gescheiterte Gates,
Rückleseberichte, manipulierte Archive/Rechte, fremde Dateien und konfliktfreie
Byte-identische Wiederholung. Es gibt keine lokale Projekt-Ausführung.

Das veröffentlichte SHA256SUMS ist eine separate Freigabe-Prüfsumme für die drei
flachen Release-Anlagen (App, Originalmanifest, test-reports.tar.gz). Die ursprüngliche
Candidate-Prüfsummendatei bleibt als test-reports/candidate-SHA256SUMS erhalten.

Auch ein vorhandener Entwurf muss unmittelbar vor der Veröffentlichung erneut
eine höhere Versionsnummer und eine höhere BUILD_NUMBER als das höchste bereits
veröffentlichte Release nachweisen. BUILD_NUMBER wird aus dessen Quell-Tag gelesen.
Ein älterer Entwurf darf nach einer neueren Veröffentlichung nicht mehr öffentlich
werden. Die vier ursprünglichen Anlagen bereits öffentlicher Releases werden
nur lesend geprüft und niemals ersetzt. Eine ausdrücklich beauftragte DMG-Ergänzung
folgt dem unten beschriebenen Verfahren. Die finale Anlagenliste muss genau den
vier ursprünglichen, zurückgelesenen Asset-IDs und gegebenenfalls den drei vollständig
geprüften DMG-Anlagen entsprechen; fremde Anlagen oder Konflikte führen zum Abbruch.
API-Zustandsfixtures decken Teil-Upload, Publish-only-Wiederholung,
veraltete Entwürfe, Build-Wiederverwendung, Konflikte und fehlerhafte Rücklesedaten ab.
Marketing-Minor 00/09 wird für Rust normalisiert (4.00 → 4.0.0; 4.09 → 4.9.0).

## DMG als bevorzugtes Installationspaket

Das Installationspaket verwendet den kurzen Namen `Switch_<version>.dmg`.
Für 3.52 ist Switch_3.52.dmg aus dem bereits geprüften App-Tarball von
d17eca3305bd2204cc895fc13af8ee0e91173f8b abzuleiten. Das vorhandene Release v3.52
und sein Tag bleiben an diesen App-Quellstand gebunden. Dies beschreibt das
beauftragte Verfahren und bestätigt noch keine erfolgte DMG-Veröffentlichung.

Die Ableitung läuft ausschließlich auf einem GitHub-macOS-Runner. Sie prüft zuerst
die ursprünglichen Release-Anlagen und ihre Quellbindung, entpackt das App-Archiv
kontrolliert und erzeugt daraus ein DMG. App-Dateien, Rechte und symbolische Links
müssen dem ursprünglichen Inventar entsprechen; Neubau und erneute Signierung der
App sind ausgeschlossen. Nach der Dateisystemprüfung wird das DMG schreibgeschützt
eingehängt. Das vollständige App-Inventar und die strenge Signaturprüfung müssen
übereinstimmen; erst nach bestätigtem Aushängen darf die Prüfung erfolgreich enden.

Die vier ursprünglichen Audit-Anlagen bleiben unverändert erhalten. Ergänzt werden
genau Switch_3.52.dmg, Switch_3.52.dmg.sha256 und dmg-provenance.json; für spätere Versionen
ändern sich die Versionsanteile von `Switch_<version>.dmg` und
`Switch_<version>.dmg.sha256`. Der Herkunftsnachweis bindet
DMG-Hash, Größe, ursprüngliches App-Archiv und Manifest an deren Quell-SHA und
Git-Tree. Davon getrennt nennt er den Commit der Verpackungssteuerung sowie ihren
GitHub-Lauf und Versuch mit den tatsächlichen Prüfberichten. Ein neuer Steuerungs-
Commit ist keine neue App-Version und darf weder App-Herkunft noch Tag umschreiben.

Auch die ergänzten Anlagen müssen nach dem Upload vollständig zurückgelesen und
geprüft werden. Wiederholungen verwenden nach Möglichkeit die bereits geprüften
DMG-Bytes; ein erneutes hdiutil-Packverfahren muss nicht byteidentisch sein.
Teil-Uploads dürfen nur mit derselben nachgewiesenen Herkunft fortgesetzt werden;
abweichende vorhandene Anlagen werden niemals überschrieben. Die Veröffentlichung
und ihre DMG-Ergänzung werden gegen gleichzeitige Schreibläufe serialisiert.
Künftige Releases sollen den DMG-Schritt automatisch an die erfolgreiche
App-Veröffentlichung anschließen. Das historische v3.51, sein Tag und seine Anlagen
bleiben vollständig unverändert.


Die Release-Fixtures prüfen zusätzlich, dass ein anderer Branch, Tag, PR-Ref
oder fehlender Branch-Kontext vor jeder API-Anfrage abgewiesen wird. Eine
abweichende head_branch-Angabe der Run-API wird vor jedem Schreibzugriff
abgewiesen. Ein während des Builds fortgeschrittenes main ändert nicht den
bereits gebundenen Quell-SHA dieses Runs.

Der einmalige schreibende reviewed-prepare-Workflow ist aus dem aktuellen
Workflow-Bestand entfernt. Frühere Autorisierung und Skript bleiben nur als
historische Nachweise erhalten. Alte Branches können weiterhin historische
Workflow-Versionen enthalten; dieser Vertrag ändert sie nicht rückwirkend.
Sie sind kein regulärer Veröffentlichungseinstieg.
