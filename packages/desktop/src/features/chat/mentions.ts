/**
 * Mention commands offered by the composer.
 *
 * The backend recognizes the same names in `packages/core/src/chat/mentions.rs`
 * and decides what a command means; this list only drives completion and the
 * menu copy. `test/mentions.test.mjs` reads the Rust list and fails when the two
 * drift, so add a command to both places.
 */
export type MentionCommand = {
  name: string;
  description: string;
};

export const MENTION_COMMANDS: MentionCommand[] = [
  {
    name: "arch",
    description: "Plan an architecture change with a LikeC4 model diff",
  },
  {
    name: "worktree",
    description: "Start this chat in a new Git worktree",
  },
  {
    name: "queue",
    description: "Send after the current turn finishes",
  },
];

export type ActiveMention = {
  /** Index of the `@` that opened the menu. */
  start: number;
  /** Index just past the token being replaced. */
  end: number;
  /** Text typed after `@`, without the sigil. */
  query: string;
};

const TOKEN_CHAR = /[A-Za-z0-9-]/;

/**
 * The mention the caret is inside, if the `@` starts a token. Requiring
 * whitespace (or the start of the message) before `@` keeps email addresses
 * from opening the menu.
 */
export function activeMention(
  text: string,
  caret: number,
): ActiveMention | null {
  if (caret < 0 || caret > text.length) return null;
  const before = text.slice(0, caret);
  const match = /(^|\s)@([A-Za-z0-9-]*)$/.exec(before);
  if (!match) return null;
  const start = caret - match[2].length - 1;
  let end = caret;
  while (end < text.length && TOKEN_CHAR.test(text[end]!)) end += 1;
  return { start, end, query: match[2] };
}

/** Commands matching the typed prefix; an empty query lists everything. */
export function mentionOptions(query: string): MentionCommand[] {
  const prefix = query.toLowerCase();
  return MENTION_COMMANDS.filter((command) =>
    command.name.startsWith(prefix),
  );
}

/** Replace the active token with `@name ` and return the new caret position. */
export function insertMention(
  text: string,
  mention: ActiveMention,
  name: string,
): { value: string; caret: number } {
  const before = text.slice(0, mention.start);
  const suffix = text.slice(mention.end);
  if (suffix.length === 0) {
    const value = `${before}@${name} `;
    return { value, caret: value.length };
  }
  // Keep the space the user already typed instead of doubling it.
  if (/^\s/.test(suffix)) {
    return {
      value: `${before}@${name}${suffix}`,
      caret: before.length + name.length + 2,
    };
  }
  return {
    value: `${before}@${name} ${suffix}`,
    caret: before.length + name.length + 2,
  };
}
