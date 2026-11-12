-- U9 A7 (Owner O3; ADR 0010, "Amendment (U9)"): who ended a Session as
-- abandoned, as the ender reported itself: its own Session, and the name
-- and role of the agent it declared. Three nullable columns appended to
-- `sessions` with ADD COLUMN only; every existing row keeps its values and
-- reads NULL in all three (a Session no ender was recorded for).
-- continuityd writes them from the same values as the Session's
-- `session.crashed@2` payload, and the integrity checks compare the two; no
-- digest covers the row.

ALTER TABLE sessions
ADD COLUMN ended_by_session_id TEXT;

ALTER TABLE sessions
ADD COLUMN ended_by_agent_name TEXT;

ALTER TABLE sessions
ADD COLUMN ended_by_agent_role TEXT;
