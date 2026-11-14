-- Auth sessions, the audit trail and per-mailbox quota usage.

CREATE TABLE sessions (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id            UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Hash of the refresh token. The raw token is never stored.
    refresh_token_hash TEXT NOT NULL UNIQUE,
    user_agent         TEXT,
    ip_address         TEXT,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at         TIMESTAMPTZ NOT NULL,
    revoked_at         TIMESTAMPTZ,
    CHECK (expires_at > created_at)
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);

-- Append-only. Actor columns are snapshots so entries survive user deletion.
CREATE TABLE audit_log (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    actor_user_id  UUID REFERENCES users (id) ON DELETE SET NULL,
    actor_email    TEXT,
    -- Dotted action name, e.g. 'auth.login', 'user.create'.
    action         TEXT NOT NULL CHECK (action <> ''),
    target_type    TEXT,
    target_id      TEXT,
    outcome        audit_outcome NOT NULL,
    ip_address     TEXT,
    user_agent     TEXT,
    details        JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX audit_log_occurred_at_idx ON audit_log (occurred_at DESC);
CREATE INDEX audit_log_actor_user_id_idx ON audit_log (actor_user_id);
CREATE INDEX audit_log_action_idx ON audit_log (action);

CREATE TABLE quota_usage (
    mailbox_id     UUID PRIMARY KEY REFERENCES mailboxes (id) ON DELETE CASCADE,
    bytes_used     BIGINT NOT NULL DEFAULT 0 CHECK (bytes_used >= 0),
    messages_count BIGINT NOT NULL DEFAULT 0 CHECK (messages_count >= 0),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
