# Menu-bar presentation regression

Run `sh scripts/test-menu-bar-presentation.sh [image-output-directory]`.

This compiles the real menu panel, label, menu presentation renderer, navigation
request center, display preferences, privacy helper, visual components and the
small status/snapshot model. The application entry point, production stores,
backend process, sockets, Keychain and launch settings controller are **not
linked**. All service doubles record actions in memory. Native offscreen
`NSHostingView` renders never launch the production application.

Coverage:

- Single dynamic menu-label Text and retained settings/update/refresh/quit paths.
- Four light/dark panel fixtures: ready, stale module with ready
  reader, absent module with ready reader, and stopped service.
- All eight signal/download/upload preference combinations.
- Masked module and reader phone numbers; navigation requests without business
  actions; stopped/stale traffic does not look live.
- Compact 320pt panel with a height ceiling, template count without a capsule.

The temporary test copy only exposes presentation helpers and uses isolated
preferences. Static contracts check preserved action definitions, while action
tests invoke the shared navigation helper; they are not automated mouse tests
of each rendered button. This does not measure CPU/network performance or
exercise real hardware behaviour.

The panel retains its compact 320pt layout and independent device states.
