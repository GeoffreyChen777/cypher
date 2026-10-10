# Cypher Edge

The Cloudflare Worker and Durable Objects behind sync, device relay,
notifications, sign-in and release downloads. `wrangler.jsonc` is the
deployment; `src/index.ts` is its entry point. Running it locally is covered in
[Local Edge development](../../docs/development/local-edge.md).

## Layout

```text
src/
  index.ts              the ordered fetch handler chain; the only place DO classes are exported
  router.ts             RouteContext, forward() into a DO, WebSocket check, shared helpers
  public-routes.ts      /health, /install.sh, /releases/*
  revoke-route.ts       POST /notifications/revoke (unauthenticated, revocation only)
  room-routes.ts        declarative table: /chat2/*, /registry/*, /device/* → Durable Objects
  blob-route.ts         GET|HEAD /blob/:chatId/:partId (read-only R2)
  identifiers.ts        identifier patterns for URL segments and notification payloads
  env.ts                the Env bindings, AUTH_USER_HEADER, json()
  blobs.ts              chunked blob storage over a DO's SQLite
  install.sh            the curl | sh installer served at /install.sh
  modules.d.ts          text-module typing for install.sh
  auth/                 bearer verification (auth.ts), /auth/* routes, the WorkOS client
  chat/                 ChatRoom DO, its SQLite log, the chat2 frame codec
  registry/             RegistryRoom DO and the pure merge core
  device/               DeviceRoom DO and its frame codec
  notifications/        the registry-side state machine, PushDevice DO, APNs sender
  legacy/               SessionRoom (410 stub, still bound) and the retired attachments PUT
test/
  workerd/              *.workerd.test.ts against real Durable Object SQLite
  support/              helpers shared by both test tiers
scripts/
  smoke.mjs             end-to-end smoke against a running Edge
  chat2-crosscheck.mjs  JS ↔ Rust chat2 convergence against a running Edge
```

The fetch handler is a chain: public routes, `/auth/*`, the revoke route, then
bearer verification (401 without it), then room routes, the blob route and the
retired attachments PUT, then 404. Each handler answers `Response | undefined`;
`undefined` passes the request to the next one. A new room sub-route is one
entry in the `ROOM_ROUTES` table in `room-routes.ts`.

The Durable Object class names and bindings in `wrangler.jsonc` are part of the
deployment: renaming or moving a class file is safe, but changing an exported
class name or a binding needs a wrangler migration.

## Conventions

On top of the TypeScript rules in
[Development](../../docs/development/README.md#typescript-appsedge-pi-runtime):

- Kebab-case file names, grouped by feature; a file keeps its basename when it
  moves, because Rust and Swift comments cite mirrored files by name.
- One Durable Object class per file, exported from `index.ts` only.
- Handlers return `Response | undefined`.
- Identifier patterns come from `identifiers.ts`; the registry wire
  validators in `registry-core.ts` stay with the code that the Rust and Swift
  clients mirror.
- Wire codecs (`chat/chat-frames.ts`, `device/device-frame.ts`) are
  import-free, so scripts load them directly through Node type stripping
  instead of keeping copies.
- `tsconfig.json` is strict, with `verbatimModuleSyntax`, `noImplicitOverride`
  and `noUncheckedIndexedAccess`; narrow with guards or early returns rather
  than `!`.
- No raw control characters in source; write `\u0000` and the like.
- Comments describe current behaviour.

## Tests

```sh
npm run typecheck   # both tsconfigs
npm test            # unit (Node) then workerd tiers
```

- Unit tests sit beside their module and are named after it
  (`src/device/device-frame.test.ts` tests `device-frame.ts`); `vitest.config.ts`
  picks up `src/**/*.test.ts`. `src/index.test.ts` holds the golden route table
  for the whole handler chain: status, namespace, room, forwarded path and
  query for every route.
- workerd tests live in `test/workerd/` and run through
  `vitest.workerd.config.ts` with real Durable Objects.
- The smoke script needs a running Edge: `npm run dev` in one terminal, then
  `node scripts/smoke.mjs` (default `http://127.0.0.1:27640`).

`bash scripts/check.sh edge` runs typecheck and both tiers, as CI does.
