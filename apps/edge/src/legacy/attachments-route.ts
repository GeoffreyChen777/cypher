/**
 * Retired attachment mirror (clients ≤0.1.62): acknowledge and discard
 * `PUT /attachments/…` so old outboxes drain once instead of retrying forever.
 */
import { json } from "../env";
import type { AuthedContext } from "../router";

export const handleRetiredAttachments = ({ request, parts }: AuthedContext): Response | undefined =>
  parts[0] === "attachments" && parts[1] && request.method === "PUT" ? json({ ok: true }) : undefined;
