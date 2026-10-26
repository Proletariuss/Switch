# GitHub-Veröffentlichungsvertrag

Neue Softwarestände erhöhen VERSION, BUILD_NUMBER und die Rust-Workspace-Version
gemeinsam. Auf 3.51 / 351 / 3.51.0 folgt 3.52 / 352 / 3.52.0. Beide Cargo-Lockfiles
müssen die lokalen Workspace-Versionen enthalten; externe Abhängigkeiten bleiben
unverändert. Bestehende Tags und Releases werden weder umgehängt noch überschrieben.

Der manuelle Einstieg ist .github/workflows/release.yml mit candidate_sha. Dieser
SHA muss dem ausgewählten Branch-Stand entsprechen. Die vollständigen gemeinsamen
Software-Prüfungen und der App-Build laufen für genau diesen Stand erneut.
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
Runs. Er erstellt einen Entwurf und lädt ausschließlich App-Tarball,
candidate-manifest.json, SHA256SUMS und test-reports.tar.gz hoch. Jede Release-
Anlage wird über die API vollständig zurückgelesen und per SHA-256 geprüft.
Erst dann werden der neue Tag auf den geprüften Commit und das öffentliche Release
angelegt. Vorhandene Entwürfe sind nur mit identischem SHA, Manifest und identischen
Asset-Bytes fortsetzbar. Konflikte führen zum Abbruch; es gibt kein clobber/retag.

Release-Titel und Hinweise sind deutsch. Die App bleibt ad-hoc signiert und nicht
notarisiert. Hardware, Mobilfunkanbieter, reale macOS-27-Ausführung und
Langzeitstabilität bleiben ohne gesonderte Nachweise ungeprüft. Kein DMG wird erzeugt.
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
werden. Bereits öffentliche, byte-identische Releases werden nur lesend geprüft.
Die finale Anlagenliste muss genau den vier zuvor zurückgelesenen Asset-IDs
entsprechen. API-Zustandsfixtures decken Teil-Upload, Publish-only-Wiederholung,
veraltete Entwürfe, Build-Wiederverwendung, Konflikte und fehlerhafte Rücklesedaten ab.
Marketing-Minor 00/09 wird für Rust normalisiert (4.00 → 4.0.0; 4.09 → 4.9.0).
