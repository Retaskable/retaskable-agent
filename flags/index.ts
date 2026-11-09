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

/** The `triage-session` flag: a session looks at stuck work and suggests what to do next. Off until there is an answer. */
export const triageSession = (): boolean => flags.on("triage-session");

/** The `cursor-give-back` flag: give a Cursor-limited item with nothing pushed back to the shared backlog. Off until there is an answer. */
export const cursorGiveBack = (): boolean => flags.on("cursor-give-back");

/** The `cursor-allow-derive` flag: Cursor sessions get an allowlist derived from the project's checks, galleries and settings. Off until there is an answer. */
export const cursorAllowDerive = (): boolean => flags.on("cursor-allow-derive");

/** The `watchdog-anomaly-rules` flag: the watchdog's newer rules (restart_wait, and idle_lanes while a restart is pending). Off until there is an answer. */
export const watchdogAnomalyRules = (): boolean => flags.on("watchdog-anomaly-rules");

/** The `engine-setup-probes` flag: probe pi and Codex at setup and say why each failed. Off until there is an answer. */
export const engineSetupProbes = (): boolean => flags.on("engine-setup-probes");

/** The `playtest-answer-command` flag: take a person's playtest answer from the site. Off until there is an answer. */
export const playtestAnswerCommand = (): boolean => flags.on("playtest-answer-command");

/** The `gallery-read-commands` flag: answer the site's requests for a gallery's pictures. Off until there is an answer. */
export const galleryReadCommands = (): boolean => flags.on("gallery-read-commands");
