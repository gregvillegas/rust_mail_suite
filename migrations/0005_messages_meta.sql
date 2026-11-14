-- Message index only. Bodies live in Maildir and are fetched over IMAP.

CREATE TABLE messages_meta (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    mailbox_id      UUID NOT NULL REFERENCES mailboxes (id) ON DELETE CASCADE,
    folder          TEXT NOT NULL DEFAULT 'INBOX',
    uid_validity    BIGINT NOT NULL CHECK (uid_validity >= 0),
    imap_uid        BIGINT NOT NULL CHECK (imap_uid > 0),
    message_id      TEXT,
    in_reply_to     TEXT,
    subject         TEXT NOT NULL DEFAULT '',
    from_addr       TEXT NOT NULL DEFAULT '',
    -- Space/comma separated address lists (kept as TEXT so they can feed the
    -- generated tsvector below; array_to_string is not immutable).
    to_addrs        TEXT NOT NULL DEFAULT '',
    cc_addrs        TEXT NOT NULL DEFAULT '',
    snippet         TEXT NOT NULL DEFAULT '',
    size_bytes      BIGINT NOT NULL DEFAULT 0 CHECK (size_bytes >= 0),
    flags           TEXT[] NOT NULL DEFAULT '{}',
    has_attachments BOOLEAN NOT NULL DEFAULT false,
    date_header     TIMESTAMPTZ,
    received_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    search_vector   TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple',
            coalesce(subject, '') || ' ' || coalesce(from_addr, '') || ' ' ||
            coalesce(to_addrs, '') || ' ' || coalesce(cc_addrs, '') || ' ' ||
            coalesce(snippet, ''))
    ) STORED,
    UNIQUE (mailbox_id, folder, uid_validity, imap_uid)
);

CREATE INDEX messages_meta_mailbox_folder_received_idx
    ON messages_meta (mailbox_id, folder, received_at DESC);
CREATE INDEX messages_meta_message_id_idx ON messages_meta (message_id);
CREATE INDEX messages_meta_search_idx ON messages_meta USING GIN (search_vector);
