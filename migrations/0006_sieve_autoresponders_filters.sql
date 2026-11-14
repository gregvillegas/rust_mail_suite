-- Per-user mail rules: Sieve scripts, autoresponders and UI-defined filters.

CREATE TABLE sieve_scripts (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    content    TEXT NOT NULL,
    is_active  BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, name)
);

-- Sieve allows exactly one active script per user.
CREATE UNIQUE INDEX sieve_scripts_one_active_idx ON sieve_scripts (user_id) WHERE is_active;

CREATE TRIGGER sieve_scripts_set_updated_at BEFORE UPDATE ON sieve_scripts
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- At most one autoresponder per user.
CREATE TABLE autoresponders (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id             UUID NOT NULL UNIQUE REFERENCES users (id) ON DELETE CASCADE,
    enabled             BOOLEAN NOT NULL DEFAULT false,
    subject             TEXT NOT NULL DEFAULT '',
    body                TEXT NOT NULL DEFAULT '',
    starts_at           TIMESTAMPTZ,
    ends_at             TIMESTAMPTZ,
    reply_interval_days INTEGER NOT NULL DEFAULT 7 CHECK (reply_interval_days > 0),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (starts_at IS NULL OR ends_at IS NULL OR ends_at > starts_at)
);

CREATE TRIGGER autoresponders_set_updated_at BEFORE UPDATE ON autoresponders
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- Rules built in the UI; compiled to Sieve (or run by the fallback engine).
-- `conditions` and `actions` are JSON documents whose schema is owned by the
-- rules engine in Phase 8.
CREATE TABLE mail_filters (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name            TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    position        INTEGER NOT NULL DEFAULT 0,
    enabled         BOOLEAN NOT NULL DEFAULT true,
    conditions      JSONB NOT NULL DEFAULT '[]'::jsonb,
    actions         JSONB NOT NULL DEFAULT '[]'::jsonb,
    stop_processing BOOLEAN NOT NULL DEFAULT false,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX mail_filters_user_position_idx ON mail_filters (user_id, position);

CREATE TRIGGER mail_filters_set_updated_at BEFORE UPDATE ON mail_filters
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
