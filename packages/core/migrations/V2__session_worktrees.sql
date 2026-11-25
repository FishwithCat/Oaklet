CREATE TABLE session_worktrees (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    state TEXT NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0
);
