# Multi-standby store regression

Compile the production MultiStandbyModels/Store with APIClient, BackendProcess,
UnixSocketURLProtocol, LocalBackendAuthentication and AppPreferences plus
`main.swift`. The injected FakeAPI performs no network or hardware operations;
notification delivery is captured in memory. Use a temporary CFFIXED_USER_HOME
under Switch/build to contain the isolated UserDefaults suites.

Covers no sender fallback, durable-intent retry identity, old profile/boot
responses, app-lifetime background notifications, first-load baseline, cursor
persistence, notification routing and cross-profile deletion rejection.
