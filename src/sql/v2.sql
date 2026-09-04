-- Record which repository created each exclusion.
-- The monitor rescans one repository at a time and must diff the exclusions that repository
-- produces against the exclusions it created, not against every cached path under its directory:
-- the latter also holds the exclusions of the nested repositories, submodules and worktrees it
-- contains.
--
-- 'repository' is nullable because the rows of the previous schema have no known owner. A full
-- scan repopulates the whole table, so they disappear at the first run.
CREATE TABLE paths_with_repository (
    path BLOB NOT NULL,
    repository BLOB,
    PRIMARY KEY (path, repository)
);

INSERT INTO paths_with_repository (path, repository) SELECT path, NULL FROM paths;

DROP TABLE paths;

ALTER TABLE paths_with_repository RENAME TO paths;

CREATE INDEX idx_paths_repository ON paths(repository);
