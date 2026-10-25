"""Rename actual IO dependencies explicitly; never rely on Swift type shadowing."""
from pathlib import Path
import sys

source, output = map(Path, sys.argv[1:])
update = (source / "UpdateChecker.swift").read_text()
update = update.replace("UserDefaults", "AcceptanceFixtureDefaults").replace("URLSession", "AcceptanceFixtureURLSession")
# Test-only visibility exposes the private didSet to explicit setter/collection
# mutation spies. No production method body or guard is replaced.
assert update.count("private var skippedVersions: Set<String>") == 1
update = update.replace("private var skippedVersions: Set<String>", "var skippedVersions: Set<String>")
(output / "UpdateChecker.swift").write_text(update)
login = (source / "AutoLaunch.swift").read_text()
login = login.replace("import ServiceManagement\n", "")
login = login.replace("SMAppService", "AcceptanceFixtureLoginService").replace("FileManager", "AcceptanceFixtureFiles")
login = login.replace("NSHomeDirectory()", "AcceptanceFixtureFiles.homeDirectory()")
login = login.replace("let process = Process()", "let process = AcceptanceFixtureProcess()")
write = "try data.write(to: URL(fileURLWithPath: launchAgentPath), options: .atomic)"
assert login.count(write) == 1
login = login.replace(write, "try AcceptanceFixtureFiles.write(data, to: launchAgentPath)")
assert "SMAppService" not in login and "NSHomeDirectory()" not in login and "Process()" not in login.replace("AcceptanceFixtureProcess()", "")
(output / "AutoLaunch.swift").write_text(login)
