# Cypher 0.1.1 — Production WorkOS/AuthKit

The production auth release: Cypher now signs in against the real Cypher
WorkOS AuthKit tenant end-to-end, on every surface — desktop, headless, and
iOS.

## What's new

- **Production WorkOS/AuthKit.** Desktop, edge, and iOS all use the production
  Cypher WorkOS client ID (`client_01M0JTKFKB6QZWHZDGYW7AN8QH`) and the edge
  verifies JWTs against the production issuer/JWKS. The old Zeron/staging
  client ID is gone from every production config; dev/local configs use
  `client_test`. The WorkOS API key lives only as a wrangler secret on the
  edge — never in the repository.
- **GitHub login.** All sign-in surfaces use the GitHub-only AuthKit flow
  (`provider=GitHubOAuth`), so the Cypher app never falls back to AuthKit's
  email/SSO selector. The pin is enforced in code and asserted in tests on
  both the engine and iOS sides.
- **PKCE everywhere (RFC 7636).** Desktop and headless (Rust engine) and iOS
  (CryptoKit/SecRandom) draw a fresh CSPRNG verifier per authorize attempt,
  publish its S256 challenge up front, and present the verifier exactly once
  at the exchange — bound to the pending OAuth `state`, so a replayed callback
  or canceled sign-in can never reuse it. The edge validates verifiers
  (`^[A-Za-z0-9\-._~]{43,128}$`) before they ever reach WorkOS.
- **iOS HTTPS callback bridge.** iOS uses `ASWebAuthenticationSession`
  against the https edge at `/auth/ios/callback`, which 302s the untouched
  query (`code`/`state`/`error`) back into the `cypher://callback` scheme. The
  bridge is a fixed redirect target — no `next` parameter, no open redirect.
- **First-user workspace creation.** An account with no memberships lands on
  an onboarding form instead of a dead-end error: name your first workspace
  and it's created (with an admin membership) and connected in one step.
- **Refresh-token single-flight & error hardening.** Desktop, headless, and
  iOS all single-flight `/auth/refresh` (a refresh token is single-use — the
  race would rotate it N times and invalidate it). The edge now returns typed
  error envelopes (`{error, code, retryable}`): only an explicit
  `invalid_grant` is a permanent 401 that revokes a session; 429/5xx/network
  failures are retryable and can never clear a device session. Refresh loops
  back off exponentially (5s → 300s), and the edge logs only a short SHA-256
  fingerprint of refresh tokens — never the token itself.
- **Production edge is locked to production credentials.** The baked
  production WorkOS client id applies ONLY when the resolved edge is exactly
  `https://edge.letscypher.app`; a custom `CYPHER_EDGE_URL` (local wrangler,
  self-hosted) disables WorkOS unless an explicit `CYPHER_WORKOS_CLIENT_ID` is
  set — a dev/self-hosted edge can never mint real production authorize URLs.
- **Ephemeral callback port.** The desktop sign-in callback now binds an
  OS-assigned loopback port by default (the dashboard registers the wildcard
  `http://127.0.0.1:*/callback`); only an explicit `CYPHER_CALLBACK_PORT` pins
  a concrete port for port-forwarded or firewalled hosts.

## macOS build note

The macOS build remains **ad-hoc signed and not notarized** unless the CI
signing secrets appear (`MACOS_CERT_P12`/`MACOS_CERT_PASSWORD` for a Developer
ID Application certificate, plus `AC_API_KEY_*` App Store Connect credentials
for notarization). When those secrets are configured, the release pipeline
signs and notarizes automatically; until then Gatekeeper may warn on the
first launch.

## Tags & artifacts

Release tag: `cypher-v0.1.1` — artifacts: `cypher-0.1.1-linux-{x86_64,aarch64}.tar.gz`,
`cypher-0.1.1-macos-arm64.dmg`, `cypher-0.1.1-macos-arm64-app.tar.gz`.

The Linux artifacts build genuinely headless (`--no-default-features`: no
X11/Wayland/GPUI linkage) and run on a clean Ubuntu 24.04 container, verified
in CI by the release smoke test; macOS ships the default headed build.
