## Change summary

Describe the problem, the intended behavior, and the smallest change that solves it.

## Validation

- [ ] The GitHub `Source contracts and formatting` check passed.
- [ ] The macOS Rust gate ran when this change affects production code.
- [ ] App-affecting changes completed the GitHub-hosted `Switch.app` build.
- [ ] No DMG, cache, runtime database, log, subscriber identity, or forwarding secret is included.
- [ ] Existing Rust backend ownership and version contracts remain intact.

## Acceptance boundary

- [ ] Software-only evidence is not described as physical-device, carrier-network, SMS, call-audio, or long-running acceptance.
- [ ] Hardware or live-network behavior that was not exercised is listed explicitly as unverified.

## Risk and rollback

State the affected lifecycle or protocol boundary, expected failure mode, and rollback commit or configuration.

> Automated dependency and maintenance pull requests must not be auto-merged. Review the diff and require all applicable checks to pass first.
