-- Refresh-token rotation (Phase 2).
--
-- Each refresh revokes the presented session and creates a successor.
-- `replaced_by` links them so that presenting an already-rotated token (a sign
-- the token was stolen and used elsewhere) can be told apart from presenting a
-- token revoked by an ordinary logout.

ALTER TABLE sessions
    ADD COLUMN replaced_by UUID REFERENCES sessions (id) ON DELETE SET NULL;
