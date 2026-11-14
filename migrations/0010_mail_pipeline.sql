-- Mail filtering pipeline (Phase 5): Amavis policy, observability, quarantine.

-- ─── Filter policies ─────────────────────────────────────────────────────────
-- Read by Amavis (through the `amavis` database role) for every recipient.
-- A scope is one of:
--     @.                  the server-wide default (exactly one row, seeded below)
--     @example.com        every recipient in that domain
--     alice@example.com   one recipient
-- Amavis merges all rows that match a recipient, most specific first, and takes
-- each setting from the first row where it is not NULL. A NULL therefore means
-- "inherit", which is why every setting is nullable.
CREATE TABLE filter_policies (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope               TEXT NOT NULL UNIQUE CHECK (
        scope = lower(scope)
        AND (scope = '@.' OR scope ~ '^@[^@[:space:]]+$' OR scope ~ '^[^@[:space:]]+@[^@[:space:]]+$')
    ),
    -- SpamAssassin score at which headers are added / a message is marked spam /
    -- a message is quarantined instead of delivered.
    spam_tag_level      NUMERIC(6, 2) CHECK (spam_tag_level IS NULL OR spam_tag_level BETWEEN -1000 AND 10000),
    spam_tag2_level     NUMERIC(6, 2) CHECK (spam_tag2_level IS NULL OR spam_tag2_level BETWEEN -1000 AND 10000),
    spam_kill_level     NUMERIC(6, 2) CHECK (spam_kill_level IS NULL OR spam_kill_level BETWEEN -1000 AND 10000),
    bypass_spam_checks  BOOLEAN,
    bypass_virus_checks BOOLEAN,
    -- "Lovers" want the mail even if it is flagged. Viruses are never delivered
    -- by default; virus_lover exists so an operator can say so explicitly.
    spam_lover          BOOLEAN,
    virus_lover         BOOLEAN,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER filter_policies_set_updated_at BEFORE UPDATE ON filter_policies
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
-- Defaults: headers from 2, "spam" from 6.2, quarantine from 12.
INSERT INTO filter_policies
    (scope, spam_tag_level, spam_tag2_level, spam_kill_level,
     bypass_spam_checks, bypass_virus_checks, spam_lover, virus_lover)
VALUES ('@.', 2.0, 6.2, 12.0, false, false, false, false);

-- ─── Pipeline events ─────────────────────────────────────────────────────────
-- One row per recipient per relevant log line, written by `rustmail-cli pipeline`.
-- Only parsed fields are stored, never the raw line or any message content.
CREATE TABLE pipeline_events (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at    TIMESTAMPTZ NOT NULL,
    source         TEXT NOT NULL CHECK (source IN ('amavis', 'postfix')),
    kind           TEXT NOT NULL CHECK (kind IN ('passed', 'blocked', 'delivery', 'reject')),
    queue_id       TEXT,
    mail_id        TEXT,
    -- Amavis verdict: CLEAN, SPAMMY, SPAM, INFECTED, BANNED, BAD-HEADER, ...
    verdict        TEXT,
    virus_name     TEXT,
    spam_score     NUMERIC(7, 2),
    sender         TEXT NOT NULL DEFAULT '',
    recipient      TEXT NOT NULL DEFAULT '',
    -- Postfix delivery status: sent, deferred, bounced.
    status         TEXT,
    detail         TEXT CHECK (detail IS NULL OR length(detail) <= 500),
    size_bytes     BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    quarantine_ref TEXT
);
CREATE INDEX pipeline_events_occurred_idx ON pipeline_events (occurred_at DESC);
CREATE INDEX pipeline_events_mail_id_idx ON pipeline_events (mail_id) WHERE mail_id IS NOT NULL;
CREATE INDEX pipeline_events_recipient_idx ON pipeline_events (recipient, occurred_at DESC);

-- Where the log follower stopped, so a restart neither repeats nor skips lines.
-- Events and the new offset are written in one transaction.
CREATE TABLE pipeline_log_state (
    path       TEXT PRIMARY KEY,
    inode      BIGINT NOT NULL CHECK (inode >= 0),
    byte_offset BIGINT NOT NULL CHECK (byte_offset >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ─── Quarantine ──────────────────────────────────────────────────────────────
-- Amavis stores the message as a file; this table is the index. One file can
-- belong to several rows, one per recipient it was held for.
CREATE TABLE quarantine_items (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    mail_id     TEXT NOT NULL,
    recipient   TEXT NOT NULL,
    sender      TEXT NOT NULL DEFAULT '',
    kind        TEXT NOT NULL CHECK (kind IN ('virus', 'spam', 'banned', 'bad-header', 'other')),
    -- Virus name, or the score, or the banned file type: what a human wants to see.
    reason      TEXT NOT NULL DEFAULT '' CHECK (length(reason) <= 500),
    spam_score  NUMERIC(7, 2),
    size_bytes  BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    -- A bare file name inside the quarantine directory: never a path.
    file_name   TEXT NOT NULL CHECK (file_name ~ '^[A-Za-z0-9][A-Za-z0-9._-]*$' AND file_name NOT LIKE '%..%'),
    received_at TIMESTAMPTZ NOT NULL,
    status      TEXT NOT NULL DEFAULT 'held' CHECK (status IN ('held', 'released', 'deleted')),
    resolved_by UUID REFERENCES users (id) ON DELETE SET NULL,
    resolved_at TIMESTAMPTZ,
    UNIQUE (mail_id, recipient),
    CHECK ((status = 'held') = (resolved_at IS NULL))
);
CREATE INDEX quarantine_items_status_received_idx ON quarantine_items (status, received_at DESC);
CREATE INDEX quarantine_items_file_idx ON quarantine_items (file_name);
CREATE INDEX quarantine_items_recipient_idx ON quarantine_items (recipient);
