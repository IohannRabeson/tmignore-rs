BEGIN TRANSACTION;

CREATE TABLE IF NOT EXISTS paths (
    path TEXT PRIMARY KEY
);

CREATE INDEX idx_paths ON paths(path);

COMMIT TRANSACTION;

-- https://www.sqlite.org/lang_analyze.html#periodically_run_pragma_optimize_
PRAGMA optimize;