-- U8 A1 (Owner O4; ADR 0010, "Amendment (U8)"): the agent a Session
-- declares at createSession: its name, its role, and the Session that
-- dispatched it. Three nullable columns appended to `sessions` with ADD
-- COLUMN only; every existing row keeps its values and reads NULL in all
-- three (a Session that declared no agent). continuityd writes them from
-- the same values as the Session's `session.started@3` payload, and the
-- integrity checks compare the two; no digest covers the row.

ALTER TABLE sessions
ADD COLUMN agent_name TEXT;

ALTER TABLE sessions
ADD COLUMN agent_role TEXT;

ALTER TABLE sessions
ADD COLUMN parent_session_id TEXT;
