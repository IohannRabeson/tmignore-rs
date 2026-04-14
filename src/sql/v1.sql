-- Create or re-create the index
-- I forgot in the first version to make it UNIQUE
DROP INDEX IF EXISTS idx_paths;
CREATE UNIQUE INDEX idx_paths ON paths(path);

CREATE TABLE metadata (
    id INTEGER PRIMARY KEY CHECK(id = 0),
    last_update BLOB
);

INSERT INTO metadata (id, last_update) VALUES(0, NULL);