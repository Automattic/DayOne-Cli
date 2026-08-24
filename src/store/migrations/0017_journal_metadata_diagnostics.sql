CREATE TABLE journal_metadata_diagnostics (
  journal_id TEXT NOT NULL,
  field TEXT NOT NULL CHECK (field IN ('name', 'description_v2')),
  status TEXT NOT NULL CHECK (status IN ('plaintext', 'malformed_d1', 'undecryptable_d1', 'unverified_d1')),
  PRIMARY KEY (journal_id, field)
);

CREATE TABLE journal_metadata_inspection (
  id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  state TEXT NOT NULL CHECK (state IN ('requested', 'running'))
);

INSERT INTO journal_metadata_inspection (id, state)
VALUES (1, 'requested');
