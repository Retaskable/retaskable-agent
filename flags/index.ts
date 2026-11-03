// Wires the flags client for this project. The owner fills in both placeholders from
// Settings → Project settings → Release flags: the project's numeric id, and its flag key
// (read from RETASKABLE_FLAG_KEY).
import { createFlagClient } from "./retaskableFlags";

declare const process: { env: Record<string, string | undefined> };

export const flags = createFlagClient({
  project: "PROJECT_ID_PLACEHOLDER", // retaskable-agent's numeric project id
  key: process.env.RETASKABLE_FLAG_KEY ?? "",
});

/** The `command-handling` flag: Continue and the other site commands. Off until there is an answer. */
export const commandHandling = (): boolean => flags.on("command-handling");

/** The `item-waits-and-finishes` flag: act on a session's `waits_on` and `nothing_left` results. Off until there is an answer. */
export const itemWaitsAndFinishes = (): boolean => flags.on("item-waits-and-finishes");

/** The `spec-drafts` flag: claiming and writing up spec drafts. Off until there is an answer. */
export const specDrafts = (): boolean => flags.on("spec-drafts");

/** The `ask-claude` flag: answering a person's chat question about the project. Off until there is an answer. */
export const askClaude = (): boolean => flags.on("ask-claude");
