DROP TRIGGER IF EXISTS events_reject_replace;
DROP TRIGGER IF EXISTS events_reject_update;
DROP TRIGGER IF EXISTS events_reject_delete;

CREATE TRIGGER events_reject_replace
BEFORE INSERT ON events
WHEN EXISTS (SELECT 1 FROM events WHERE event_id = NEW.event_id)
OR EXISTS (
    SELECT 1 FROM events
    WHERE session_id = NEW.session_id AND sequence = NEW.sequence
)
OR EXISTS (SELECT 1 FROM events WHERE event_sha256 = NEW.event_sha256)
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_reject_update
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_reject_delete
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;
