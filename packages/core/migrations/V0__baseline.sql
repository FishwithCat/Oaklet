-- Baseline schema. Add new changes as V1__desc.sql, V2__desc.sql, and never
-- edit a migration that has already been applied.
CREATE TABLE sessions (
  id             TEXT PRIMARY KEY,
  workspace      TEXT NOT NULL,
  title          TEXT NOT NULL DEFAULT '',
  turns          INTEGER NOT NULL DEFAULT 0,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  context_used   INTEGER,
  context_window INTEGER
);
CREATE INDEX sessions_by_workspace ON sessions(workspace, updated_at DESC);

CREATE TABLE session_events (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  seq        INTEGER NOT NULL,
  payload    TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (session_id, seq)
);

CREATE TABLE runs (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  status      TEXT NOT NULL,
  started_at  INTEGER NOT NULL,
  finished_at INTEGER,
  error       TEXT
);
CREATE INDEX runs_by_session ON runs(session_id, started_at DESC);
