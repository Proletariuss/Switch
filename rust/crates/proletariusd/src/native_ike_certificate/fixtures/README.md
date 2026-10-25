These files are synthetic offline test credentials, never subscriber or service keys.
`rsa.test-pem` and `ec.test-pem` are deliberately public test signing keys. Their
only callers are cfg(test) loopback fixtures. Production uses default macOS trust,
never these certificates or injected anchors, and never changes a system keychain.

The test root is RSA 2048 / SHA256, CA=true, keyCertSign+cRLSign. Leaves are RSA
2048 and named-curve P-256, SHA256-signed by that root. Leaves have digitalSignature,
IPsec IKE/server/client EKUs, SAN DNS epdg.test and IP 127.0.0.1 / ::1. Explicit
curve encoding was rejected by macOS trust and is intentionally not used.

Generated locally with /usr/bin/openssl req, ecparam, and x509. Root validity runs
from 2026-10-13 through 2046-10-08; RSA leaf 2026-10-13 through 2036-10-10, EC leaf
likewise. Tests preserve real validity checks and do not pin or alter system time.
These checked-in DER bytes keep test identity/signing reproducible throughout
that validity interval. Regenerate all certificates together before expiry.
The root private signing key is not part of source. For regeneration create a
fresh offline root, then leaves using the extensions above; ecparam must use
-param_enc named_curve. Record their real dates and rerun all trust/wire tests.

Test signatures are generated over each actual captured SA_INIT response, nonce
and IDr-derived PRF. /usr/bin/openssl is only a test fixture signer, not a new
production dependency, trust provider, or runtime network process.
