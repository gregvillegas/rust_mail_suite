-- Hosted mail domains, user accounts (login identity) and their mailboxes
-- (storage). Domain names and e-mail addresses are stored lower-case; the
-- application normalises input and the CHECK constraints are a backstop.

CREATE TABLE domains (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name                TEXT NOT NULL UNIQUE
                        CHECK (name = lower(name) AND name ~ '^[a-z0-9.-]+$' AND position('.' in name) > 1),
    status              account_status NOT NULL DEFAULT 'active',
    -- NULL means unlimited.
    default_quota_bytes BIGINT CHECK (default_quota_bytes IS NULL OR default_quota_bytes >= 0),
    max_mailboxes       INTEGER CHECK (max_mailboxes IS NULL OR max_mailboxes >= 0),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER domains_set_updated_at BEFORE UPDATE ON domains
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE users (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain_id          UUID NOT NULL REFERENCES domains (id) ON DELETE RESTRICT,
    local_part         TEXT NOT NULL CHECK (local_part = lower(local_part) AND length(local_part) BETWEEN 1 AND 64),
    -- Denormalised local_part@domain, used for login lookups. Domains are
    -- never renamed, so this cannot drift.
    email              TEXT NOT NULL UNIQUE CHECK (email = lower(email) AND position('@' in email) > 1),
    -- Argon2id PHC string (hashing arrives with the auth layer in Phase 2).
    password_hash      TEXT NOT NULL,
    display_name       TEXT NOT NULL DEFAULT '',
    role               user_role NOT NULL DEFAULT 'user',
    status             account_status NOT NULL DEFAULT 'active',
    locale             TEXT NOT NULL DEFAULT 'en',
    last_login_at      TIMESTAMPTZ,
    failed_login_count INTEGER NOT NULL DEFAULT 0 CHECK (failed_login_count >= 0),
    locked_until       TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (domain_id, local_part)
);

CREATE INDEX users_domain_id_idx ON users (domain_id);
CREATE INDEX users_role_idx ON users (role);

CREATE TRIGGER users_set_updated_at BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- One primary mailbox per user. Shared mailboxes are out of scope for now.
CREATE TABLE mailboxes (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id       UUID NOT NULL UNIQUE REFERENCES users (id) ON DELETE CASCADE,
    -- Maildir location relative to the vmail base directory.
    maildir_path  TEXT NOT NULL UNIQUE CHECK (maildir_path <> '' AND maildir_path NOT LIKE '%..%'),
    -- NULL means "inherit the domain default"; resolved by the application.
    quota_bytes   BIGINT CHECK (quota_bytes IS NULL OR quota_bytes >= 0),
    status        account_status NOT NULL DEFAULT 'active',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER mailboxes_set_updated_at BEFORE UPDATE ON mailboxes
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
