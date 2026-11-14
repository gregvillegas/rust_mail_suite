-- Stub tables for the groupware phase (Phase 9). Only the basics are defined
-- here so ownership and uniqueness rules are settled early; sharing/ACLs and
-- CalDAV/CardDAV sync metadata are added later.

CREATE TABLE calendar_events (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_user_id  UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- iCalendar UID, stable across clients.
    uid            TEXT NOT NULL,
    title          TEXT NOT NULL DEFAULT '',
    description    TEXT NOT NULL DEFAULT '',
    location       TEXT NOT NULL DEFAULT '',
    starts_at      TIMESTAMPTZ NOT NULL,
    ends_at        TIMESTAMPTZ NOT NULL,
    all_day        BOOLEAN NOT NULL DEFAULT false,
    rrule          TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (owner_user_id, uid),
    CHECK (ends_at >= starts_at)
);

CREATE INDEX calendar_events_owner_start_idx ON calendar_events (owner_user_id, starts_at);

CREATE TRIGGER calendar_events_set_updated_at BEFORE UPDATE ON calendar_events
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE contacts (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_user_id  UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- vCard UID, stable across clients.
    uid            TEXT NOT NULL,
    full_name      TEXT NOT NULL DEFAULT '',
    email          TEXT NOT NULL DEFAULT '',
    phone          TEXT NOT NULL DEFAULT '',
    organization   TEXT NOT NULL DEFAULT '',
    vcard          TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (owner_user_id, uid)
);

CREATE INDEX contacts_owner_idx ON contacts (owner_user_id);

CREATE TRIGGER contacts_set_updated_at BEFORE UPDATE ON contacts
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
