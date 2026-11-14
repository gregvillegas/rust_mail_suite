-- Webmail search index bookkeeping (Phase 6).
--
-- `messages_meta` holds one row per message the webmail has indexed (headers and a
-- short snippet, never the full message). This table remembers how far each folder
-- has been indexed so a sync only fetches what is new, and notices when the IMAP
-- server renumbers a folder (UIDVALIDITY changes), which invalidates every UID.
CREATE TABLE mail_sync_state (
    mailbox_id   UUID NOT NULL REFERENCES mailboxes (id) ON DELETE CASCADE,
    folder       TEXT NOT NULL CHECK (folder <> '' AND length(folder) <= 512),
    uid_validity BIGINT NOT NULL CHECK (uid_validity >= 0),
    last_uid     BIGINT NOT NULL CHECK (last_uid >= 0),
    synced_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (mailbox_id, folder)
);

-- Full-text search tokenises "bob@example.com" as ONE token, so a search for "bob" or
-- "example" would miss it. Index the text twice: as written, and with punctuation
-- turned into spaces. Queries are normalised the same way (see `mailindex::search_terms`).
DROP INDEX messages_meta_search_idx;
ALTER TABLE messages_meta DROP COLUMN search_vector;
ALTER TABLE messages_meta ADD COLUMN search_vector TSVECTOR GENERATED ALWAYS AS (
    to_tsvector('simple',
        coalesce(subject, '') || ' ' || coalesce(from_addr, '') || ' ' ||
        coalesce(to_addrs, '') || ' ' || coalesce(cc_addrs, '') || ' ' || coalesce(snippet, '') || ' ' ||
        translate(
            coalesce(subject, '') || ' ' || coalesce(from_addr, '') || ' ' ||
            coalesce(to_addrs, '') || ' ' || coalesce(cc_addrs, ''),
            '@._-<>",;:()[]', '              '))
) STORED;
CREATE INDEX messages_meta_search_idx ON messages_meta USING GIN (search_vector);
