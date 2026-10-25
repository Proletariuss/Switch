# Offline reader-page presentation check

Run from the repository root:

```sh
sh scripts/test-reader-presentation.sh build/reader-presentation
```

This compiles and renders the real `ReaderCardView` with production value models,
carrier formatting, shared components and image assets. Its service dependencies
are in-memory doubles. It neither launches the application nor links the real
backend, PC/SC, Bluetooth, network, notification or Keychain services. Profile and
phone identities are synthetic. Rendering asserts that the actual `onAppear`
synchronization runs and that no card/device operation is invoked.

The ten PNG fixtures cover 2 and 9 profiles, a long personalized name, masking
on/off, light/dark appearance, pending USIM identity, and 688×550 / 1040×800
viewports. `measurements.json` records full content height and pixels below the
initial viewport. Profile disclosure is explicitly expanded for both baseline
and current rendering. These tests do not open child sheets or certify their
behavior.

The communication slot deliberately uses a labeled, inert 210-point layout
fixture, not the real IMS panel. Its measurements describe reader-page layout
only; they do not certify real VoWiFi registration, eSIM operations, animations,
accessibility or end-to-end app layout. Screenshots require visual review.

For baseline comparison, supply a read-only extracted old view alongside its
matching `HomeView.swift` and `HomeSupportViews.swift`; no production file is
replaced:

```sh
READER_PRESENTATION_VIEW_SOURCE=/absolute/path/to/old/ReaderCardView.swift \
  sh scripts/test-reader-presentation.sh build/reader-presentation-baseline
```

The actual reader page's running-state composition is extracted from that
version of `HomeView`, including its hero and the old `WaterfallLayout` when
applicable. The baseline shares current common components and the same
communication fixture. It isolates reader layout changes, not every historical
theme difference. Both versions receive the fixed viewport content width; this
also avoids the old waterfall layout's non-finite intrinsic-size-proposal trap.
That historical crash was observed during the initial baseline render; the now
unused production type has been removed, rather than patched inside the old
baseline to manufacture layout results. The script accepts both the old
non-generic composition and the new communication-slot composition. All generated Swift, asset catalogs and
the test-only app bundle are temporary and removed on exit.
