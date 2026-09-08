# Cloudflare Workers → APNs transport validation

Verified on **2026-09-07** using a temporary **remote Cloudflare preview**,
not local Node.js/workerd and not the production `cypher-edge` Worker.

## Result

Workers' native `fetch()` reached the APNs provider API in both environments
and received Apple authentication responses with distinct `apns-id` headers.
Workers Web Crypto also generated and self-verified an ES256 JWT signature.
The lack of a functional `node:http2` module does **not** require a separate
APNs sending server for this application.

This is a transport/cryptographic-primitive validation, **not successful
authenticated delivery to an iPhone**. No real APNs private key or device
token was used.

## Setup and isolation

- Wrangler 4.119.0, `wrangler dev --remote`.
- Compatibility date `2026-07-01`, matching the existing Edge configuration.
- No `nodejs_compat`, third-party APNs package, resource bindings, routes, or
  production secrets.
- Separate temporary Worker name and config; local proxy bound to loopback.
- Cloud execution reported `colo: DUB`.
- Fixed `/3/device/` path using 64 zeroes instead of a real device token.
- POST alert JSON with the Cypher topic and `apns-expiration: 0`.
- Native `fetch()`, manual redirects and a 15-second request deadline.

Do not run this experiment with the production Wrangler config: remote
previews can access bound production resources. Use an isolated config.

## Observations

Four cloud probe rounds, two APNs endpoints per round, eight requests total:

| Request | `api.push.apple.com` | `api.sandbox.push.apple.com` |
| --- | --- | --- |
| No Authorization header (three rounds) | `403 {"reason":"MissingProviderToken"}` | Same |
| ES256 JWT signed with a newly generated, **unregistered test key** | `403 {"reason":"InvalidProviderToken"}` | Same |

Every response had an Apple `apns-id`. Representative IDs:

- Production, no credentials: `33AF0152-480E-8A65-4780-C677A0514933`
- Sandbox, no credentials: `FD67E24B-9160-A864-CD4A-32C7790D60C1`
- Production, invalid test credential: `7ED3C669-B4A1-3BA5-0BD8-1EE87DED8DB4`
- Sandbox, invalid test credential: `2316B6FF-5574-8AF2-02B2-79120C1F659C`

These 403 responses were the expected negative-authentication outcomes:
Apple parsed the provider request and rejected missing/invalid authorization,
rather than the request failing to reach the provider API.

The test JWT used a non-extractable, freshly generated P-256 key in the Worker.
`crypto.subtle.sign` produced a 64-byte ES256 signature; verification with the
generated public key succeeded. The JWT/private key was not returned or logged.

### Protocol control

The same unauthenticated production request from the cloud Mac:

- `curl --http1.1`: failed with `Received HTTP/0.9 when not allowed`, no HTTP status.
- `curl --http2`: HTTP/2, `403 MissingProviderToken`.

The Worker response's `incomingProtocol: HTTP/1.1` describes the request
**from the preview proxy to the Worker**, not the APNs subrequest. Fetch does
not expose the upstream HTTP version here; do not mislabel that field as
evidence about APNs transport.

The initial Python urllib requests to the preview front door received
Cloudflare error 1010. Using a normal browser User-Agent for the local
preview request resolved that front-door issue. No special User-Agent or
protocol override was needed on the Worker → APNs fetch.

## Cleanup

The isolated Wrangler preview process was stopped. A subsequent Cloudflare
deployments query returned **10007: This Worker does not exist on your
account**, confirming no permanent Worker deployment was created.
The production Worker and its bindings/routes were not modified.

Private local evidence:

```text
/tmp/cypher-apns-worker-probe-path
  → /private/tmp/cypher-apns-worker-probe.w7zbyY
```

This directory contains the probe source/config, four JSON responses,
local protocol comparison, private Wrangler logs and the summary. It is not
a deployed notification service.

## What remains before claiming push delivery

1. Enable Push Notifications for the iOS App ID and ship matching entitlements.
2. Configure a real, approved APNs key as a Worker secret, with the correct
   team/key ID and topic. Do not reuse an App Store Connect API key.
3. Register a real APNs device token with the correct production/sandbox
   environment and authenticated account binding.
4. Verify an authorized APNs request returns 200, then verify receipt,
   foreground behavior and tap navigation on a real device.
5. Implement/test the approved notification policy, deduplication, retries,
   invalid-token cleanup and account-switch protections.

Keep the design on **Cloudflare Workers + APNs** unless a future, evidenced
limitation requires a different transport.
