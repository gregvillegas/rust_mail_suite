-- Calendar and contacts (Phase 9): CalDAV / CardDAV storage, sharing and app passwords.
--
-- A collection is a calendar or an address book. An object is one event, task or contact,
-- stored as the exact text the client sent (so nothing it wrote is ever lost) plus a few
-- extracted columns used for queries. Every change bumps the collection's `revision`;
-- CalDAV's sync-token is that number, and `dav_tombstones` remembers deletions so a client
-- that syncs later can be told what disappeared.

-- Passwords for calendar and contact apps (Thunderbird, phones). The user's login password
-- is never typed into those apps. Only a SHA-256 of the token is stored: tokens are 128
-- random bits, so a fast hash is enough.
CREATE TABLE app_passwords (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 64),
    token_hash   TEXT NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at   TIMESTAMPTZ
);
CREATE INDEX app_passwords_user_idx ON app_passwords (user_id);

CREATE TABLE dav_collections (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('calendar', 'addressbook')),
    -- The last part of the collection's URL.
    slug         TEXT NOT NULL CHECK (slug ~ '^[a-z0-9][a-z0-9._-]{0,62}$'),
    display_name TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 128),
    description  TEXT NOT NULL DEFAULT '' CHECK (length(description) <= 1000),
    color        TEXT CHECK (color IS NULL OR color ~ '^#[0-9A-Fa-f]{6}$'),
    revision     BIGINT NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (owner_id, kind, slug)
);

CREATE TABLE dav_objects (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    collection_id UUID NOT NULL REFERENCES dav_collections (id) ON DELETE CASCADE,
    -- The last part of the object's URL (for example "5f3a.ics").
    name          TEXT NOT NULL CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9._@=+-]{0,200}$'),
    uid           TEXT NOT NULL CHECK (length(uid) BETWEEN 1 AND 255),
    etag          TEXT NOT NULL,
    data          TEXT NOT NULL CHECK (length(data) <= 1048576),
    component     TEXT NOT NULL CHECK (component IN ('VEVENT', 'VTODO', 'VCARD')),
    summary       TEXT NOT NULL DEFAULT '',
    -- Words searched in the contacts list (name, organisation, emails, phone numbers).
    search_text   TEXT NOT NULL DEFAULT '',
    -- Time extent of events: the first start and the end of the last occurrence.
    -- `last_end` is NULL for a repeating event that never ends.
    starts_at     TIMESTAMPTZ,
    ends_at       TIMESTAMPTZ,
    last_end      TIMESTAMPTZ,
    recurring     BOOLEAN NOT NULL DEFAULT false,
    -- Whether the event blocks time in free/busy (not TRANSPARENT, not CANCELLED).
    busy          BOOLEAN NOT NULL DEFAULT true,
    revision      BIGINT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (collection_id, name),
    UNIQUE (collection_id, uid)
);
CREATE INDEX dav_objects_range_idx ON dav_objects (collection_id, starts_at, last_end);
CREATE INDEX dav_objects_revision_idx ON dav_objects (collection_id, revision);

CREATE TABLE dav_tombstones (
    collection_id UUID NOT NULL REFERENCES dav_collections (id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    revision      BIGINT NOT NULL,
    PRIMARY KEY (collection_id, name, revision)
);

-- Sharing: a collection is shared with one user or one group, read-only or read/write.
CREATE TABLE dav_shares (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    collection_id    UUID NOT NULL REFERENCES dav_collections (id) ON DELETE CASCADE,
    grantee_user_id  UUID REFERENCES users (id) ON DELETE CASCADE,
    grantee_group_id UUID REFERENCES groups (id) ON DELETE CASCADE,
    can_write        BOOLEAN NOT NULL DEFAULT false,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((grantee_user_id IS NULL) <> (grantee_group_id IS NULL))
);
-- What a user may do with a collection, in one place so every query agrees:
-- 3 = owner, 2 = may read and write, 1 = may read, 0 = no access (NULL if there is no such collection).
-- A group share counts for each member of the group; the highest level wins.
CREATE FUNCTION dav_access(p_user UUID, p_collection UUID) RETURNS INTEGER
LANGUAGE sql STABLE AS $$
    SELECT CASE
        WHEN c.owner_id = p_user THEN 3
        ELSE COALESCE((
            SELECT max(CASE WHEN s.can_write THEN 2 ELSE 1 END)
            FROM dav_shares s
            WHERE s.collection_id = c.id
              AND (s.grantee_user_id = p_user
                   OR s.grantee_group_id IN (SELECT gm.group_id FROM group_members gm WHERE gm.user_id = p_user))
        ), 0)
    END
    FROM dav_collections c WHERE c.id = p_collection
$$;

CREATE UNIQUE INDEX dav_shares_user_idx ON dav_shares (collection_id, grantee_user_id) WHERE grantee_user_id IS NOT NULL;
CREATE UNIQUE INDEX dav_shares_group_idx ON dav_shares (collection_id, grantee_group_id) WHERE grantee_group_id IS NOT NULL;
CREATE INDEX dav_shares_grantee_user_idx ON dav_shares (grantee_user_id);
CREATE INDEX dav_shares_grantee_group_idx ON dav_shares (grantee_group_id);
