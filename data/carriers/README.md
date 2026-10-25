# Carrier source data

The current source corpus is pinned to
[`iniwex5/ios-carrier-profiles@09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25`](https://github.com/iniwex5/ios-carrier-profiles/tree/09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25).
The 676 profile YAML files, 853 selector YAML files, license, and other upstream
files have exact hashes in the [source manifest](../../docs/optimization/all-rust/carrier-reference-locks/ios-carrier-profiles-09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25-manifest.json).
The MIT terms are preserved in [ios-carrier-profiles-MIT.txt](ios-carrier-profiles-MIT.txt).

The [compatibility overlay](compatibility/registry-overlay.json) retains the
historical profiles and selector decisions that the upstream revision removed
or changed. It is separately attributed to the previous `cdba73e` source
snapshot. The generated Rust registry combines the current upstream corpus
with that explicit overlay; source-file counts therefore differ from registry
counts. [Iteration 547](../../docs/optimization/all-rust/iteration-547-carrier-refresh.md)
records the exact selection and effective-configuration comparison. These are
configuration and offline-compatibility results, not claims that every
operator's live VoWiFi service was tested.

SIM World is treated as the user's 3HK 454/03 subscription. The scoped
[3HK user template](user_templates/3_hk.json), SHA-256
`25a3279dfa6836a217dd714a54a024f882403df4e9c4593a819e5c9894527992`,
is unchanged by the source refresh. Its provenance and precedence are detailed
in [SOURCES.md](SOURCES.md). That file also retains the historical source
records and limitations from earlier revisions.
