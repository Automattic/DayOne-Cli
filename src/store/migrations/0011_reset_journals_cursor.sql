-- Re-fetch the global journals feed once so clients that previously advanced
-- past journal tombstones while deletion handling was incomplete can reconcile
-- deleted server journals locally.
DELETE FROM sync_cursors
WHERE resource_key = 'journals';
