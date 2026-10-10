/**
 * SessionRoom — retired. The pre-chat2 session rooms (`s2/{chatId}`) and the
 * pre-registry workspace rooms (`ws4/{orgId}/{userId}`) are no longer routed;
 * chat2 (ChatRoom) and the registry (RegistryRoom) replaced them.
 *
 * The class stays exported and bound (`SESSION_ROOMS`) for one more release
 * so the deployed namespace and its stored data are not destroyed. Deleting
 * it needs a `deleted_classes` migration in wrangler.jsonc, which permanently
 * drops that storage.
 */
export class SessionRoom implements DurableObject {
  async fetch(): Promise<Response> {
    return new Response(JSON.stringify({ error: "gone" }), {
      status: 410,
      headers: { "content-type": "application/json" }
    });
  }

  /** A hibernated room may still hold a scheduled daily alarm; consume it
   * without re-arming. */
  async alarm(): Promise<void> {}
}
