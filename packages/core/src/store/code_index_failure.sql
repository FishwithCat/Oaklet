-- Test-only fault after replacement deletes the old symbol rows.
CREATE TEMP TRIGGER fail_symbol BEFORE INSERT ON code_index_symbols
WHEN NEW.name = 'fail'
BEGIN
  SELECT RAISE(ABORT, 'injected symbol write failure');
END;
