-- Mail filters and the autoresponder (Phase 8).
--
-- Phase 1 created `mail_filters`, `sieve_scripts` and `autoresponders` for this phase;
-- nothing has used them yet, so they can still be shaped to what the UI needs:
--   mail_filters   one row per rule (the rule builder); `stop_processing` is the "stop" action
--   sieve_scripts  the user's own script from the advanced editor; the active row, if any,
--                  replaces the generated one
--   autoresponders the vacation reply, with calendar dates (Sieve compares dates, not instants)
-- The Sieve scripts Dovecot runs are generated from these rows and pushed over
-- ManageSieve; `sieve_sync_state` records whether the last push worked, so the UI can say
-- when a change is not live yet.
ALTER TABLE mail_filters ADD COLUMN match_all BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE mail_filters ADD CONSTRAINT mail_filters_conditions_array CHECK (jsonb_typeof(conditions) = 'array');
ALTER TABLE mail_filters ADD CONSTRAINT mail_filters_actions_array CHECK (jsonb_typeof(actions) = 'array');
-- Filters are applied in position order; a position is unique per user.
DROP INDEX mail_filters_user_position_idx;
CREATE UNIQUE INDEX mail_filters_user_position_idx ON mail_filters (user_id, position);
ALTER TABLE sieve_scripts ADD CONSTRAINT sieve_scripts_size CHECK (length(content) <= 65536);

ALTER TABLE autoresponders DROP CONSTRAINT autoresponders_check;
ALTER TABLE autoresponders DROP COLUMN starts_at, DROP COLUMN ends_at;
ALTER TABLE autoresponders
    ADD COLUMN starts_on DATE,
    ADD COLUMN ends_on DATE,
    ADD CONSTRAINT autoresponders_dates CHECK (starts_on IS NULL OR ends_on IS NULL OR ends_on >= starts_on),
    ADD CONSTRAINT autoresponders_interval_max CHECK (reply_interval_days <= 30),
    ADD CONSTRAINT autoresponders_text_size CHECK (length(subject) <= 1000 AND length(body) <= 32000);

CREATE TABLE sieve_sync_state (
    user_id      UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    script       TEXT NOT NULL CHECK (script IN ('filters', 'vacation')),
    installed_at TIMESTAMPTZ,
    error        TEXT,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, script)
);
