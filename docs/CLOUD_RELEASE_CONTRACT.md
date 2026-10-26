# GitHub-Veröffentlichungsvertrag

Neue Softwarestände erhöhen VERSION, BUILD_NUMBER und die Rust-Workspace-Version
gemeinsam. Auf 3.51 / 351 / 3.51.0 folgt 3.52 / 352 / 3.52.0. Beide Cargo-Lockfiles
müssen die lokalen Workspace-Versionen enthalten; externe Abhängigkeiten bleiben
unverändert. Bestehende Tags und App-Quellstände werden weder umgehängt noch ersetzt.

Der manuelle Einstieg für einen neuen Tag ist .github/workflows/release.yml mit
candidate_sha. Dieser SHA muss dem ausgewählten Branch-Stand entsprechen. Die
vollständigen gemeinsamen Software-Prüfungen und der App-Build laufen für genau
diesen Stand erneut. Ein älterer erfolgreicher Lauf ist keine Freigabe für eine
neue Versionsnummer. Der vollständige Ablauf ist: Softwareprüfung und App-Build,
geprüfte App-Veröffentlichung, DMG-Ableitung, Sicherung der Audit-Anlagen und
abschließende Reduktion auf genau zwei hochgeladene Installationsdateien.

## App-Prüfung und vorläufige Audit-Anlagen

Vor dem Packen prüft verify-app-identity.py das App-Plist und die eingebettete
Backend-Identität einschließlich Quell-Digest. Das ist keine Laufzeitabnahme.
Der SHA-haltige App-Tarball erhält Rechte und symbolische Links. Erst nach dem
tatsächlichen Upload, erneuten Download und erfolgreicher Byte-/Inventarprüfung
darf der getrennte publish-Job Schreibrechte verwenden. PR-Jobs erhalten diese
Berechtigung nicht. Ein Release-Einstieg erzeugt vor diesen Prüfungen keinen Tag.

Der Publisher prüft Quellen-SHA, Git-Tree, Version, Build, Run und ursprünglichen
Candidate-Versuch; er akzeptiert nur erfolgreiche Originalberichte. Ein reiner
Publish-Wiederholungsversuch vor der abschließenden Bereinigung verwendet den
bereits geprüften Candidate desselben Runs. Der vorläufige Entwurf enthält vier
Audit-Anlagen: App-Tarball, candidate-manifest.json, SHA256SUMS und test-reports.tar.gz.
Jede Anlage wird über die API vollständig zurückgelesen und per SHA-256 geprüft.
Erst dann werden der neue Tag auf den geprüften Commit und das öffentliche Release
angelegt. Vorhandene Entwürfe sind nur mit identischem SHA, Manifest und identischen
Asset-Bytes fortsetzbar. Konflikte führen zum Abbruch; es gibt kein clobber/retag.

Auch ein vorhandener Entwurf muss unmittelbar vor der Veröffentlichung erneut
eine höhere Versionsnummer und eine höhere BUILD_NUMBER als das höchste bereits
veröffentlichte Release nachweisen. BUILD_NUMBER wird aus dessen Quell-Tag gelesen.
Ein älterer Entwurf darf nach einer neueren Veröffentlichung nicht mehr öffentlich
werden. Marketing-Minor 00/09 wird für Rust normalisiert (4.00 → 4.0.0; 4.09 → 4.9.0).

SHA256SUMS bindet die drei flachen Audit-Dateien App, Originalmanifest und
test-reports.tar.gz. Die ursprüngliche Candidate-Prüfsummendatei bleibt als
test-reports/candidate-SHA256SUMS im Testarchiv erhalten. Diese Audit-Dateien sind
Zwischennachweise und werden vor ihrer Entfernung aus dem Release in Actions gesichert.

## DMG als Installationspaket

Das Installationspaket heißt `Switch_<version>.dmg`, seine Prüfsummendatei
`Switch_<version>.dmg.sha256`. Switch_3.52.dmg wurde aus dem bereits geprüften
App-Tarball von d17eca3305bd2204cc895fc13af8ee0e91173f8b abgeleitet. Release v3.52
und sein Tag bleiben an diesen App-Quellstand gebunden. Die App-Prüfung ist in
[Run 40314576638](https://github.com/Proletariuss/Switch/actions/runs/40314576638)
dokumentiert; die erfolgreiche DMG-Prüfung mit Verpackungssteuerung
0f3e9277071cc942382586b677d0c52fc5413115 in
[Run 40325760479](https://github.com/Proletariuss/Switch/actions/runs/40325760479).

Die Ableitung läuft ausschließlich auf einem GitHub-macOS-Runner. Sie prüft zuerst
die ursprünglichen Audit-Anlagen einschließlich ihrer Actions-Herkunft, entpackt
das App-Archiv kontrolliert und erzeugt daraus ein DMG. App-Dateien, Rechte und
symbolische Links müssen dem ursprünglichen Inventar entsprechen; Neubau und
erneute Signierung der App sind ausgeschlossen. Nach der Dateisystemprüfung wird
das DMG schreibgeschützt eingehängt. Das vollständige App-Inventar und die strenge
Signaturprüfung müssen übereinstimmen; erst nach bestätigtem Aushängen darf die
Prüfung erfolgreich enden.

Vor der Bereinigung kommen genau DMG, zugehörige .dmg.sha256 und dmg-provenance.json
zu den vier ursprünglichen Audit-Anlagen hinzu. Der Herkunftsnachweis bindet
DMG-Hash, Größe, ursprüngliches App-Archiv und Manifest an deren Quell-SHA und
Git-Tree. Davon getrennt nennt er den Commit der Verpackungssteuerung sowie ihren
GitHub-Lauf und Versuch. Ein neuer Steuerungs-Commit ist keine neue App-Version
und darf weder App-Herkunft noch Tag umschreiben.

Auch die ergänzten Anlagen müssen nach dem Upload vollständig zurückgelesen und
geprüft werden. Wiederholungen vor der Bereinigung verwenden die bereits geprüften
DMG-Bytes; ein erneutes hdiutil-Packverfahren muss nicht byteidentisch sein.
Teil-Uploads dürfen nur mit derselben nachgewiesenen Herkunft fortgesetzt werden;
abweichende vorhandene Anlagen werden niemals überschrieben. Die Zwischenliste
enthält genau die vier ursprünglichen und die drei geprüften DMG-Anlagen mit ihren
zurückgelesenen Asset-IDs; fremde Anlagen oder Konflikte führen zum Abbruch.

## Abschließende Bereinigung und Belege

Der Endzustand enthält genau zwei hochgeladene Release-Anlagen:
Switch_3.52.dmg und Switch_3.52.dmg.sha256; für neue Versionen ändern sich nur
die Versionsanteile. Vor dem ersten Löschen werden die fünf Audit-Anlagen
App-Tarball, candidate-manifest.json, test-reports.tar.gz, SHA256SUMS und
dmg-provenance.json unverändert als Actions-Artefakt hochgeladen und zurückgeprüft.
Der Bereinigungsnachweis hält Quell- und Verpackungsherkunft, ursprüngliche
Asset-Identitäten und Prüfsummen sowie die beiden verbleibenden Dateien fest.
Eine fehlgeschlagene Sicherung erlaubt keine Löschung.

Gelöscht werden ausschließlich die vorab geprüften fünf Audit-Asset-IDs des
vorgesehenen Release. Die beiden Installationsdateien werden weder ersetzt noch
umbenannt. Bei teilweise gelöschten Anlagen oder vollständig gelöschten Audit-Dateien
ohne bereits geschriebene Bereinigungsnotiz wird ausschließlich der ursprüngliche
fehlgeschlagene prune-Job erneut ausgeführt. Er verwendet denselben vollständigen
Actions-Snapshot und Löschplan; bereits entfernte erlaubte Anlagen bleiben entfernt.
Ein neuer Dispatch lehnt diese unvollständigen Zustände ab. Sind nur noch die beiden
Installationsdateien vorhanden und ist die Bereinigungsnotiz bereits geschrieben,
bleibt eine Wiederholung am Release lesend und erhält den ursprünglichen
Snapshot-Verweis. Fremde Anlagen, abweichende IDs oder Inhalte führen zum Abbruch.
Abschließend werden die beiden verbleibenden Dateien und die Release-/Tag-Bindung
erneut geprüft.

Ein bereinigtes Release ist ein Endzustand. Alte vollständige App-Publisher oder
DMG-Packläufe dürfen dafür nicht erneut gestartet werden; insbesondere dürfen sie
keine entfernten Audit-Anlagen wiederherstellen. Nur die Bereinigung selbst darf
für denselben nachgewiesenen Stand wiederholt werden. Neue Tags durchlaufen den
vollständigen Ablauf automatisch bis zum Endzustand mit zwei Dateien.
Veröffentlichung, DMG-Ergänzung und Bereinigung werden gegen gleichzeitige
Schreibläufe serialisiert. Das historische v3.51, sein Tag und seine Anlagen
bleiben vollständig unverändert.

GitHub bietet zusätzlich automatisch „Source code (zip)“ und „Source code (tar.gz)“
an. Diese Links gehören nicht zu den hochgeladenen Release-Assets und bleiben nach
der Bereinigung sichtbar; sie sind keine Installationspakete. Siehe
[GitHub: About releases](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases).
Actions-Nachweise sind nicht unbegrenzt verfügbar: Ihre Aufbewahrung richtet sich
nach den tatsächlichen Repository-/Organisations-Einstellungen und dem jeweiligen
Ablaufdatum. Eine feste Frist oder dauerhafte Speicherung wird nicht zugesichert.

## Abnahmegrenzen

Release-Titel und Hinweise sind deutsch. Die App bleibt ad hoc signiert und nicht
notarisiert. Hardware, Mobilfunkanbieter, reale macOS-27-Ausführung und
Langzeitstabilität bleiben ohne gesonderte Nachweise ungeprüft. DMG-Verpackung und
Bereinigung ändern diese Grenzen nicht. Ein Release verändert oder merged keinen
Quellbranch.

Offline-Fixtures und Projektprüfungen laufen ausschließlich auf GitHub-Runnern.
Sie prüfen Versions-/Quellbindung, gescheiterte Gates, Rückleseberichte,
manipulierte Archive/Rechte, fremde Dateien, Teil-Uploads, Wiederholungen und
Konflikte. Es gibt keine lokale Projekt-Ausführung.
