-- Derived metadata only; deleting an index must never touch conversation data.
CREATE TABLE code_index_projects (
  id INTEGER PRIMARY KEY,
  workspace TEXT NOT NULL,
  root TEXT NOT NULL,
  language TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  format_version INTEGER NOT NULL,
  revision INTEGER NOT NULL,
  snapshot TEXT NOT NULL,
  verified INTEGER NOT NULL,
  last_access INTEGER NOT NULL,
  UNIQUE(workspace, root, language)
);
CREATE TABLE code_index_files (
  id INTEGER PRIMARY KEY,
  project_id INTEGER NOT NULL REFERENCES code_index_projects(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  size INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  content_hash TEXT NOT NULL,
  extracted_hash TEXT,
  symbols_state TEXT NOT NULL,
  evidence_state TEXT NOT NULL,
  UNIQUE(project_id, path)
);
CREATE TABLE code_index_symbols (
  file_id INTEGER NOT NULL REFERENCES code_index_files(id) ON DELETE CASCADE,
  project_id INTEGER NOT NULL REFERENCES code_index_projects(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  name TEXT NOT NULL,
  search_name TEXT NOT NULL,
  kind INTEGER NOT NULL,
  container TEXT,
  payload TEXT NOT NULL,
  PRIMARY KEY(file_id, ordinal)
);
CREATE INDEX code_index_symbols_by_name ON code_index_symbols(project_id, search_name);
CREATE TABLE code_index_evidence (
  file_id INTEGER NOT NULL REFERENCES code_index_files(id) ON DELETE CASCADE,
  project_id INTEGER NOT NULL REFERENCES code_index_projects(id) ON DELETE CASCADE,
  command TEXT NOT NULL,
  category TEXT NOT NULL,
  payload TEXT NOT NULL,
  extraction_version INTEGER NOT NULL
);
CREATE INDEX code_index_evidence_by_file ON code_index_evidence(file_id);
CREATE INDEX code_index_evidence_by_command ON code_index_evidence(project_id, command);
CREATE TABLE code_index_queries (
  key TEXT PRIMARY KEY,
  project_id INTEGER NOT NULL REFERENCES code_index_projects(id) ON DELETE CASCADE,
  snapshot TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  format_version INTEGER NOT NULL,
  payload TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_access INTEGER NOT NULL
);
CREATE INDEX code_index_queries_by_access ON code_index_queries(last_access);
