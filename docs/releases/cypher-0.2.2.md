# Cypher 0.2.2 — GitHub email verification

A patch for first-time GitHub sign-in through WorkOS, with a polished browser
callback experience.

## What's new

- **New GitHub users can finish email verification.** When WorkOS requires a
  six-digit email code after GitHub OAuth, Cypher now preserves the pending
  authentication securely and completes the verification instead of failing
  during token exchange.
- **Designed callback pages.** The browser verification, success, cancellation,
  and error pages now use a responsive Cypher dark theme with a focused OTP
  input and clear retry states.
- **Desktop, CLI, and iOS coverage.** Desktop users verify in the local browser
  callback, `cypher login` prompts in the terminal, and iOS presents an in-app
  verification form.
- **Updated website favicon.** The landing page tab icon now matches the current
  Cypher app icon.

## Tags & artifacts

Release tag: `cypher-v0.2.2` (immutable) — artifacts:
`cypher-0.2.2-linux-{x86_64,aarch64}.tar.gz`, `cypher-0.2.2-macos-arm64.dmg`,
`cypher-0.2.2-macos-arm64-app.tar.gz`.
