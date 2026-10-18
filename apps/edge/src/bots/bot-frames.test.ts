import { describe, expect, it } from "vitest";
import vectorFile from "../../../../protocol/vectors/bot-frames-v1.json";
import { parseBotFrame, splitInviteCode } from "./bot-frames";

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
