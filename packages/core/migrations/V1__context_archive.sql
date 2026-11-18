-- Recall is an index of immutable original events, independent of context size.
CREATE TABLE archived_messages (
  session_id TEXT NOT NULL,
  event_seq INTEGER NOT NULL,
  PRIMARY KEY (session_id, event_seq),
  FOREIGN KEY (session_id, event_seq) REFERENCES session_events(session_id, seq) ON DELETE CASCADE
);
CREATE INDEX session_events_by_type
  ON session_events(session_id, json_extract(payload, '$.type'), seq);
INSERT OR IGNORE INTO archived_messages
SELECT m.session_id, m.seq FROM session_events m
WHERE json_extract(m.payload, '$.type') = 'message'
  AND (m.seq <= COALESCE((SELECT MAX(json_extract(c.payload, '$.shadowed_through'))
    FROM session_events c WHERE c.session_id = m.session_id
      AND json_extract(c.payload, '$.type') = 'compaction'), 0)
  OR m.seq IN (SELECT json_extract(t.payload, '$.original_seq') FROM session_events t
    WHERE t.session_id = m.session_id AND json_extract(t.payload, '$.type') = 'tool_trim'));
