-- Test-only fault: derived query writes fail while file metadata and chat remain usable.
CREATE TRIGGER fail_query_write BEFORE INSERT ON code_index_queries
BEGIN
  SELECT RAISE(ABORT, 'injected query cache write failure');
END;
