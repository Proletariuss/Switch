# Iteration 614 — 9eSIM 6982 scope boundary

The user explicitly directed that the 9eSIM encrypted profile-download path returning `6982` no longer be processed. This path is excluded from code repair and acceptance; keep the returned error available for diagnosis, do not bypass card authentication, and do not describe the path as fixed.

The exclusion is narrow. Retain B0/B1 card-password and BF71 authentication, BLE connection, EID/profile reading, profile switching, and supported normal authenticated downloads that do not use the excluded path. The generic LPA feature and other supported card/carrier flows remain in scope.

This versioned clarification changes no requirement leaf IDs and awards no acceptance. It changes one NINE_ESIM scenario member; the overall denominator remains unfrozen.
