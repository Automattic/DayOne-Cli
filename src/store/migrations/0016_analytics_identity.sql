-- Preserve the Tracks identity that was active when each event was queued.
-- Legacy rows remain NULL and use the current identity as a best-effort fallback.
ALTER TABLE analytics_events ADD COLUMN tracks_ui TEXT;
ALTER TABLE analytics_events ADD COLUMN tracks_ut TEXT;
