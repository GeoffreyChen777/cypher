# Smart mobile notifications

## Implementation and rollout status

The implementation lives in the existing Cloudflare Worker, desktop app and iOS
client. There is **no separate APNs sender service**. Native Workers `fetch()`
calls APNs; `jose` signs ES256 provider tokens using Workers Web Crypto.
See [the cloud transport validation](apns-workers-validation.md).

Production remains **disabled** by `NOTIFICATIONS_ENABLED = "false"`.
Source entitlements are not Apple Developer portal configuration. No real APNs
key has been configured by this implementation, and neither authenticated APNs
acceptance nor physical-device delivery has been verified. The already uploaded
TestFlight 0.1.3 (1) does not contain these changes.

## Behavior

The iOS Home menu opens **Notifications**. Preferences are shared within the
authenticated user/organization, with per-Project muting.

| Mode | Behavior |
| --- | --- |
| Smart (default) | Suppress normal phone alerts during active desktop use; defer unresolved input requests until away. |
| Needs attention | Input requests and failures, subject to their switches; no ordinary completion. |
| Always | Ignore desktop activity suppression, but still respect event switches, project muting and viewed-chat cancellation. |
| Off | Do not send or display new in-app alerts. |

- Eligible events: completion, failure and waiting for input.
- No output-token, tool-call or progress notifications. Completions under 30
  seconds are skipped when run duration is known.
- Independent child results default off. A child asking for input can still
  notify. Child navigation resolves against actual synchronized chat records.
  A parent's async launch acknowledgement does not announce completion while
  its snapshot still contains running async children. The later aggregate
  terminal snapshot produces a fresh parent result (including child failure),
  rather than backfilling the old launch acknowledgement.
- An event waits about 10 seconds. Starting another run, resolving the question,
  archiving/deleting the chat, muting it or viewing it can cancel the event.
- A suppressed ordinary completion/failure is dropped, **not backfilled** later.
  An unresolved question can notify once after the desktop activity lease expires.
- Same chat visible on iPhone: silent. Another page visible: a short in-app
  banner, not a second system banner. Background delivery uses a normal APNs
  alert. iOS Focus and notification permissions continue to govern presentation.
- Lock-screen content is generic, e.g. “Task completed / Open Cypher to view the
  session.” No project name, prompt, transcript, tool arguments or provider keys
  are included.

“Active desktop” means a foreground Cypher window and interaction within roughly
two minutes. Keyboard/mouse/scroll callbacks record only the interaction time:
**not keys, coordinates or content**. Reports renew every 15 seconds, with a
45-second server lease and immediate reporting for focus/chat changes or resumed
interaction. Per-client sequences reject reordered reports. Online headless
engines and ordinary registry presence are deliberately not activity signals.
Older desktop clients cannot suppress phone notifications: update desktop clients
before enabling the service.

## Server and account boundaries

- Authenticated `/registry/:org/notifications/{settings,activity,register,unregister}`
  routes select `reg1/<org>/<user>` from verified authentication, not a supplied
  user header. JSON bodies have a streaming 16 KiB limit.
- `Notifications` stores preferences, activity, recipients and a bounded durable
  outbox in RegistryRoom SQLite. It consumes session transitions based on the
  row's execution-owner `deviceId`, not the device that replicated the row;
  this preserves notifications for iOS-started remote runs. Baseline snapshots,
  unchanged replay and stale transitions are still ignored.
- Notification alarms share scheduling with daily registry backup/GC. A
  notification alarm must not continually postpone the backup deadline.
- Events are rechecked against current run/status, project/chat existence and
  preferences immediately before sending. Normal events expire after 10 minutes;
  unresolved input expires after eight hours. Retry uses bounded exponential
  backoff. One alarm processes at most two events and 16 recipients per event.
- `PushDevice` is global per APNs token/environment. Installation IDs, monotonic
  epochs and random leases prevent old registrations and delayed logout requests
  from overriding a newer owner. Retired installation watermarks do not expire.
  Registration and delivery are serialized through the ownership check.
- Successful sends are deduplicated per event/device, with stable APNs collapse
  IDs. This is **not an exactly-once delivery guarantee**: a crash between Apple's
  acceptance and recording the receipt can cause a retry. APNs collapse IDs and
  the iOS seen-event cache reduce that window.
- Invalid APNs tokens disable the registration. Revocation and invalidation
  discard the raw token while retaining replay-prevention metadata.
- iOS stores registration/revocation capabilities in `ThisDeviceOnly` Keychain
  storage. An offline logout queues revocation, which can be retried without an
  old account's bearer/refresh token.
- `/notifications/revoke` is an unauthenticated **revocation-only capability**
  endpoint: exact opaque binding ID, scope, lease and newer epoch are required.
  It cannot register, read or send. Apply normal public-endpoint abuse/rate
  controls during rollout.
- Account/config generations guard in-flight HTTP work, registration responses
  and notification navigation. Invalidated auth configurations cannot persist a
  late refresh over a newly signed-in account.
- A tap can wait for cold-start authentication/sync, but only opens an actual
  chat in the matching current workspace and project. No URL from the payload
  is followed.

Apple may already have accepted a notification when logout/muting occurs. Those
in-flight system alerts cannot be recalled reliably; their text stays generic,
and cross-account taps are rejected. APNs acceptance also does not prove device
receipt or presentation.

## Enabling delivery — separate approval required

1. Enable Push Notifications for the existing App ID
   `ai.mvp-lab.cypher.ios`, team `999875MHT4`. Do not create a replacement bundle.
2. Create/use an **APNs token-auth key**, authorized for this topic and the desired
   APNs environment. This is not an App Store Connect API key.
3. Store the following as Cloudflare secrets using a private interactive/file
   workflow, never chat, shell command arguments, git or logs:
   `APNS_PRIVATE_KEY` (PKCS#8 `.p8` contents), `APNS_KEY_ID`, `APNS_TEAM_ID`.
   The current sender uses one configured provider key; ensure it is authorized
   for every environment you intend to use.
4. Review the `PushDevice` SQLite migration/binding (`v4`) and deployment plan.
   Keep the production Worker name and existing Durable Object identities.
   Do not enable outgoing request tracing: the APNs URL contains the device
   token. Update App Store privacy disclosures for identifiers and coarse
   activity used for notification functionality; the required-reason API
   manifest alone is not an App Privacy declaration.
5. Produce a newly authorized iOS build. Debug uses `development` (sandbox);
   Release/TestFlight uses `production`. **Verify the exported IPA's actual
   signed `aps-environment=production` entitlement**, not just its Info.plist.
   Revalidate distribution export after the portal capability change.
   A cloud Mac does not need a registered physical development device for
   App Store/TestFlight distribution.
6. Deploy/enable only with approval and complete real-device acceptance below.
   Code commit, push, deployment, setting the flag and TestFlight upload remain
   separate operations. Pushes to main can trigger the production deploy workflow.

No phone Runtime, provider credential or MCP installation is required.
Transport uses TLS; this is not a claim of end-to-end encryption.

## Tests and acceptance

Local suites cover policy/validation, authenticated routing, APNs payload/retry
classification, real workerd SQLite outbox persistence, token ownership,
desktop activity sampling, iOS registration/permission boundaries, in-app
suppression, cold-start scope checks, and auth-refresh invalidation.

```sh
cd edge
npm run typecheck
npm run test:unit
npm run test:workerd
npm run build                 # deploy --dry-run only
```

For iOS Keychain tests, use **local ad-hoc signing** on the simulator:

```sh
export DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer
xcodebuild -project apps/ios/Cypher.xcodeproj -scheme Cypher \
  -destination 'platform=iOS Simulator,id=<isolated-dev-simulator>' \
  -parallel-testing-enabled NO \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- test
```

`CODE_SIGNING_ALLOWED=NO` produces an unsigned test host whose real Keychain
writes fail with `errSecMissingEntitlement (-34018)`. Do not skip the Keychain
tests or mistake that environment failure for an auth race.

Real-device rollout must separately verify:

- permission refusal/grant and token registration;
- production APNs HTTP 200 **and actual phone receipt/tap**;
- desktop active versus away, and resuming interaction before the delay expires;
- same-chat silence, other-page in-app banner and background/system presentation;
- resolved input cancellation and one deferred unresolved-input reminder;
- subagent defaults, project muting and all modes;
- offline logout, account/organization switch, token rotation and cold start;
- reconnect/restart without a flood of old completion notifications;
- iOS Focus behavior and exported distribution entitlements.

Mock/SQLite tests and a successful cloud transport probe do not replace this
acceptance.
