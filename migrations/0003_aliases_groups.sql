-- Virtual aliases (Postfix virtual_alias_maps) and groups (distribution lists).

-- One row per (address, destination) pair, so an alias can fan out to
-- several destinations. Destinations may be internal or external addresses.
CREATE TABLE aliases (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain_id   UUID NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    address     TEXT NOT NULL CHECK (address = lower(address) AND position('@' in address) > 1),
    destination TEXT NOT NULL CHECK (destination = lower(destination) AND position('@' in destination) > 1),
    enabled     BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (address, destination),
    CHECK (address <> destination)
);

CREATE INDEX aliases_domain_id_idx ON aliases (domain_id);
CREATE INDEX aliases_address_idx ON aliases (address) WHERE enabled;

CREATE TABLE groups (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    domain_id   UUID NOT NULL REFERENCES domains (id) ON DELETE CASCADE,
    name        TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    -- Optional distribution-list address that fans out to all members.
    address     TEXT UNIQUE CHECK (address IS NULL OR (address = lower(address) AND position('@' in address) > 1)),
    description TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (domain_id, name)
);

CREATE TRIGGER groups_set_updated_at BEFORE UPDATE ON groups
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TABLE group_members (
    group_id   UUID NOT NULL REFERENCES groups (id) ON DELETE CASCADE,
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    added_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, user_id)
);

CREATE INDEX group_members_user_id_idx ON group_members (user_id);
