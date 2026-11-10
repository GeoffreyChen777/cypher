import { describe, expect, it } from "vitest";
import vectorFile from "../../../../protocol/vectors/bot-frames-v1.json";
import { avatarUrl, parseBotFrame, splitInviteCode } from "./bot-frames";

/** The bot network frames are a cross-language contract: the Rust engine
 * runs the same vectors (protocol/vectors/bot-frames-v1.json,
 * protocol/README.md). */

interface Vectors {
  frames: { name: string; frame: unknown }[];
  invalid: { name: string; frame: unknown }[];
  codes: { code: string; room: string | null; secret: string | null }[];
}

const vectors = vectorFile as Vectors;

describe("bot frames (shared vectors)", () => {
  it.each(vectors.frames.map((c) => [c.name, c.frame] as const))("parses %s unchanged", (_name, frame) => {
    expect(parseBotFrame(frame)).toEqual(frame);
  });

  it.each(vectors.invalid.map((c) => [c.name, c.frame] as const))("rejects %s", (_name, frame) => {
    expect(parseBotFrame(frame)).toBeUndefined();
  });

  it.each(vectors.codes.map((c) => [c.code, c] as const))("splits invite code %j", (_code, c) => {
    const split = splitInviteCode(c.code);
    expect(split ?? null).toEqual(c.room === null || c.secret === null ? null : [c.room, c.secret]);
  });
});

describe("owner photos", () => {
  it.each([
    "https://avatars.githubusercontent.com/u/123?v=4",
    "https://lh3.googleusercontent.com/a/ACg8oc",
    "https://www.gravatar.com/avatar/abc?d=identicon"
  ])("keeps %s", (url) => expect(avatarUrl(url)).toBe(url));
  it.each([
    "http://avatars.githubusercontent.com/u/1",
    "https://evil.example/pixel.png",
    "https://githubusercontent.com.evil.example/a.png",
    "https://evilgravatar.com/a.png",
    "https://user@avatars.githubusercontent.com/u/1",
    "https://avatars.githubusercontent.com:8443/u/1",
    "https://avatars.githubusercontent.com/u/1 x",
    ""
  ])("drops %j", (url) => expect(avatarUrl(url)).toBeUndefined());
  it("drops a peer's photo off the avatar hosts", () => {
    const frame = parseBotFrame({
      t: "link",
      connectionId: "c",
      op: "added",
      peer: { botRoomId: "r", name: "Ledger", ownerName: "Dana", ownerAvatarUrl: "https://evil.example/p.png" }
    });
    expect(frame).toEqual({ t: "link", connectionId: "c", op: "added", peer: { botRoomId: "r", name: "Ledger", ownerName: "Dana" } });
  });
});
