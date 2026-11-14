-- Quota accounting shared with Dovecot (Phase 4).
--
-- Dovecot's SQL dictionary addresses a row by *user name* (the e-mail address)
-- and inserts or increments it itself. RustMail addresses it by mailbox id.
-- Rather than keep two tables in sync, the one table carries both keys and a
-- trigger fills in whichever is missing, so there is a single source of truth.

ALTER TABLE quota_usage ADD COLUMN username TEXT;

UPDATE quota_usage q
SET username = u.email
FROM mailboxes m
JOIN users u ON u.id = m.user_id
WHERE m.id = q.mailbox_id;

ALTER TABLE quota_usage ALTER COLUMN username SET NOT NULL;
ALTER TABLE quota_usage ADD CONSTRAINT quota_usage_username_key UNIQUE (username);

-- Dovecot adds *signed* deltas with INSERT ... VALUES (-n) ON CONFLICT DO UPDATE,
-- and PostgreSQL checks CHECK constraints on that proposed row before it resolves
-- the conflict. A CHECK (>= 0) would therefore reject every decrement, i.e. every
-- deleted message. Clamping in a trigger gives the same guarantee (usage never goes
-- negative) without ever failing the caller.
ALTER TABLE quota_usage DROP CONSTRAINT quota_usage_bytes_used_check;
ALTER TABLE quota_usage DROP CONSTRAINT quota_usage_messages_count_check;

CREATE FUNCTION quota_usage_link() RETURNS trigger AS $$
BEGIN
    -- Usernames are e-mail addresses and are stored lower-case everywhere.
    IF NEW.username IS NOT NULL THEN
        NEW.username := lower(NEW.username);
    END IF;

    IF NEW.mailbox_id IS NULL AND NEW.username IS NOT NULL THEN
        SELECT m.id INTO NEW.mailbox_id
        FROM mailboxes m JOIN users u ON u.id = m.user_id
        WHERE u.email = NEW.username;
    ELSIF NEW.username IS NULL AND NEW.mailbox_id IS NOT NULL THEN
        SELECT u.email INTO NEW.username
        FROM mailboxes m JOIN users u ON u.id = m.user_id
        WHERE m.id = NEW.mailbox_id;
    ELSIF NEW.username IS NOT NULL AND NEW.mailbox_id IS NOT NULL THEN
        -- Both keys given: they must name the same mailbox.
        IF NOT EXISTS (
            SELECT 1 FROM mailboxes m JOIN users u ON u.id = m.user_id
            WHERE m.id = NEW.mailbox_id AND u.email = NEW.username
        ) THEN
            RAISE EXCEPTION 'quota_usage: username and mailbox_id refer to different mailboxes'
                USING ERRCODE = 'foreign_key_violation';
        END IF;
    END IF;

    NEW.bytes_used := GREATEST(NEW.bytes_used, 0);
    NEW.messages_count := GREATEST(NEW.messages_count, 0);

    -- Refuse rows for users that have no mailbox instead of storing orphans.
    IF NEW.mailbox_id IS NULL OR NEW.username IS NULL THEN
        RAISE EXCEPTION 'quota_usage: no mailbox for the given user or mailbox id'
            USING ERRCODE = 'foreign_key_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- BEFORE INSERT runs ahead of the NOT NULL and uniqueness checks, which is what
-- lets Dovecot insert with a username alone.
CREATE TRIGGER quota_usage_link BEFORE INSERT ON quota_usage
    FOR EACH ROW EXECUTE FUNCTION quota_usage_link();

CREATE FUNCTION quota_usage_clamp() RETURNS trigger AS $$
BEGIN
    NEW.bytes_used := GREATEST(NEW.bytes_used, 0);
    NEW.messages_count := GREATEST(NEW.messages_count, 0);
    NEW.updated_at := now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER quota_usage_clamp BEFORE UPDATE ON quota_usage
    FOR EACH ROW EXECUTE FUNCTION quota_usage_clamp();
