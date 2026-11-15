//! Collections, objects, sharing and free/busy.
//!
//! Every method takes the acting user's id and checks their access to the collection
//! itself (`dav_access` in the database: owner, read/write or read-only). A collection
//! the user has no access to is reported as not found, so its existence is not revealed.

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::calendar::{self, Occurrence};
use super::contact;
use super::{DavError, DavResult, MAX_OBJECT_BYTES};

/// Most objects in one collection.
pub const MAX_OBJECTS: i64 = 20_000;
/// Most collections of one kind a user may own.
pub const MAX_COLLECTIONS: i64 = 50;
/// Longest time window free/busy and the calendar view accept.
pub const MAX_WINDOW_DAYS: i64 = 400;

/// What a user may do with a collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    /// Read only.
    Read,
    /// Read and change objects.
    ReadWrite,
    /// Everything, including renaming, deleting and sharing.
    Owner,
}

impl Access {
    fn from_level(level: i32) -> Option<Self> {
        match level {
            1 => Some(Self::Read),
            2 => Some(Self::ReadWrite),
            3 => Some(Self::Owner),
            _ => None,
        }
    }

    /// Whether objects may be added, changed and removed.
    #[must_use]
    pub fn can_write(self) -> bool {
        self >= Self::ReadWrite
    }

    /// Whether this is the owner.
    #[must_use]
    pub fn is_owner(self) -> bool {
        self == Self::Owner
    }
}

/// A calendar or an address book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Holds events and tasks.
    Calendar,
    /// Holds contacts.
    AddressBook,
}

impl Kind {
    /// The value stored in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calendar => "calendar",
            Self::AddressBook => "addressbook",
        }
    }

    fn parse(s: &str) -> Self {
        if s == "addressbook" {
            Self::AddressBook
        } else {
            Self::Calendar
        }
    }
}

/// A collection and what the asking user may do with it.
#[derive(Debug, Clone)]
pub struct Collection {
    /// Identifier.
    pub id: Uuid,
    /// The owner.
    pub owner_id: Uuid,
    /// The owner's address (part of the collection's URL).
    pub owner_email: String,
    /// Calendar or address book.
    pub kind: Kind,
    /// Last part of the URL.
    pub slug: String,
    /// Name shown to people.
    pub display_name: String,
    /// Longer description, or empty.
    pub description: String,
    /// Colour as `#rrggbb`, if set.
    pub color: Option<String>,
    /// Changes so far; the sync token is built from it.
    pub revision: i64,
    /// What the asking user may do.
    pub access: Access,
}

/// An object without its text: enough to list it and compare it.
#[derive(Debug, Clone)]
pub struct ObjectMeta {
    /// Last part of the URL.
    pub name: String,
    /// Changes whenever the text does.
    pub etag: String,
    /// The object's UID.
    pub uid: String,
    /// `VEVENT`, `VTODO` or `VCARD`.
    pub component: String,
    /// Title or name.
    pub summary: String,
    /// The collection revision at which it last changed.
    pub revision: i64,
    /// Size of the text in bytes.
    pub size: i64,
}

/// An object with its text.
#[derive(Debug, Clone)]
pub struct Object {
    /// Listing fields.
    pub meta: ObjectMeta,
    /// The iCalendar or vCard text, exactly as stored.
    pub data: String,
}

/// A condition on a write (`If-Match` / `If-None-Match`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    /// No condition.
    None,
    /// Only if the object does not exist yet.
    Absent,
    /// Only if the object exists with this etag.
    Etag(String),
}

/// The result of storing an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The object's etag now.
    pub etag: String,
    /// Whether it did not exist before.
    pub created: bool,
}

/// What changed in a collection since a revision.
#[derive(Debug, Clone)]
pub struct Changes {
    /// The collection's revision now (the next sync token).
    pub revision: i64,
    /// Objects added or changed.
    pub changed: Vec<ObjectMeta>,
    /// Names of objects removed.
    pub deleted: Vec<String>,
}

/// Who a collection is shared with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grantee {
    /// One user.
    User {
        /// The user's id.
        id: Uuid,
        /// The user's address.
        email: String,
    },
    /// Every member of a group.
    Group {
        /// The group's id.
        id: Uuid,
        /// The group's name.
        name: String,
    },
}

/// One entry of a collection's share list.
#[derive(Debug, Clone)]
pub struct Share {
    /// Identifier, used to remove it.
    pub id: Uuid,
    /// With whom.
    pub grantee: Grantee,
    /// Whether they may change objects.
    pub can_write: bool,
}

/// Who to share with, as given in a form.
#[derive(Debug, Clone)]
pub enum GranteeRef {
    /// A user's address.
    UserEmail(String),
    /// A group's id.
    Group(Uuid),
}

/// Changes to a collection's settings. `None` leaves a field alone.
#[derive(Debug, Clone, Default)]
pub struct CollectionUpdate {
    /// New display name.
    pub display_name: Option<String>,
    /// New description.
    pub description: Option<String>,
    /// New colour; `Some(None)` removes it.
    pub color: Option<Option<String>>,
}

/// A stretch of time someone is busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    /// Start.
    pub start: DateTime<Utc>,
    /// End.
    pub end: DateTime<Utc>,
}

/// One occurrence for the calendar page, with where it lives.
#[derive(Debug, Clone)]
pub struct CalendarEvent {
    /// The calendar holding it.
    pub collection_id: Uuid,
    /// The calendar's colour.
    pub color: Option<String>,
    /// The object's name in that calendar.
    pub object: String,
    /// Whether the user may change it.
    pub writable: bool,
    /// When and what.
    pub occurrence: Occurrence,
}

/// The calendar and contacts database.
#[derive(Clone)]
pub struct DavStore {
    pool: PgPool,
}

/// An object name is the last part of its URL.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 201
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '=' | '+' | '-'))
}

/// A collection slug is the last part of its URL.
fn valid_slug(slug: &str) -> bool {
    let mut chars = slug.chars();
    slug.len() <= 63
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// Makes a URL-safe slug from a display name.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out
        .trim_end_matches('-')
        .chars()
        .take(40)
        .collect::<String>();
    if out.is_empty() {
        "collection".to_owned()
    } else {
        out
    }
}

fn etag_of(data: &str) -> String {
    Sha256::digest(data.as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn check_color(color: &str) -> DavResult<()> {
    let ok = color.len() == 7
        && color.starts_with('#')
        && color[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(DavError::Invalid(
            "the colour must look like #3366cc".into(),
        ))
    }
}

fn check_text(s: &str, what: &str, max: usize, required: bool) -> DavResult<()> {
    if (required && s.trim().is_empty())
        || s.chars().count() > max
        || s.chars().any(|c| c.is_control() && c != '\n')
    {
        return Err(DavError::Invalid(format!(
            "the {what} must be {}{max} characters of plain text",
            if required { "1 to " } else { "at most " }
        )));
    }
    Ok(())
}

impl DavStore {
    /// Creates the store.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Gives a user a first calendar and address book if they have none.
    ///
    /// # Errors
    /// Database errors.
    pub async fn ensure_defaults(&self, user_id: Uuid) -> DavResult<()> {
        sqlx::query!(
            r#"INSERT INTO dav_collections (owner_id, kind, slug, display_name)
               SELECT $1, 'calendar', 'default', 'Calendar'
               WHERE NOT EXISTS (SELECT 1 FROM dav_collections WHERE owner_id = $1 AND kind = 'calendar')
               ON CONFLICT DO NOTHING"#,
            user_id
        )
        .execute(&self.pool)
        .await?;
        sqlx::query!(
            r#"INSERT INTO dav_collections (owner_id, kind, slug, display_name)
               SELECT $1, 'addressbook', 'contacts', 'Contacts'
               WHERE NOT EXISTS (SELECT 1 FROM dav_collections WHERE owner_id = $1 AND kind = 'addressbook')
               ON CONFLICT DO NOTHING"#,
            user_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The user's own collections of `kind`, then those shared with them.
    ///
    /// # Errors
    /// Database errors.
    pub async fn collections(&self, user_id: Uuid, kind: Kind) -> DavResult<Vec<Collection>> {
        let rows = sqlx::query!(
            r#"SELECT c.id, c.owner_id, o.email AS owner_email, c.kind, c.slug, c.display_name, c.description, c.color, c.revision,
                      COALESCE(dav_access($1, c.id), 0) AS "level!"
               FROM dav_collections c JOIN users o ON o.id = c.owner_id
               WHERE c.kind = $2 AND COALESCE(dav_access($1, c.id), 0) > 0
               ORDER BY (c.owner_id <> $1), lower(o.email), c.display_name, c.slug"#,
            user_id,
            kind.as_str()
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(Collection {
                    id: r.id,
                    owner_id: r.owner_id,
                    owner_email: r.owner_email,
                    kind: Kind::parse(&r.kind),
                    slug: r.slug,
                    display_name: r.display_name,
                    description: r.description,
                    color: r.color,
                    revision: r.revision,
                    access: Access::from_level(r.level)?,
                })
            })
            .collect())
    }

    /// One collection by its id.
    ///
    /// # Errors
    /// [`DavError::NotFound`] if it does not exist or the user has no access.
    pub async fn collection(&self, user_id: Uuid, id: Uuid) -> DavResult<Collection> {
        self.fetch_collection(user_id, None, id).await
    }

    /// One collection by its place in the URL: owner address, kind and slug.
    ///
    /// # Errors
    /// [`DavError::NotFound`] if it does not exist or the user has no access.
    pub async fn find(
        &self,
        user_id: Uuid,
        owner_email: &str,
        kind: Kind,
        slug: &str,
    ) -> DavResult<Collection> {
        let id = sqlx::query_scalar!(
            r#"SELECT c.id FROM dav_collections c JOIN users o ON o.id = c.owner_id
               WHERE lower(o.email) = lower($1) AND c.kind = $2 AND c.slug = $3"#,
            owner_email,
            kind.as_str(),
            slug
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(DavError::NotFound)?;
        self.fetch_collection(user_id, Some(kind), id).await
    }

    async fn fetch_collection(
        &self,
        user_id: Uuid,
        kind: Option<Kind>,
        id: Uuid,
    ) -> DavResult<Collection> {
        let r = sqlx::query!(
            r#"SELECT c.id, c.owner_id, o.email AS owner_email, c.kind, c.slug, c.display_name, c.description, c.color, c.revision,
                      COALESCE(dav_access($1, c.id), 0) AS "level!"
               FROM dav_collections c JOIN users o ON o.id = c.owner_id WHERE c.id = $2"#,
            user_id,
            id
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(DavError::NotFound)?;
        let access = Access::from_level(r.level).ok_or(DavError::NotFound)?;
        let found = Kind::parse(&r.kind);
        if kind.is_some_and(|k| k != found) {
            return Err(DavError::NotFound);
        }
        Ok(Collection {
            id: r.id,
            owner_id: r.owner_id,
            owner_email: r.owner_email,
            kind: found,
            slug: r.slug,
            display_name: r.display_name,
            description: r.description,
            color: r.color,
            revision: r.revision,
            access,
        })
    }

    /// Makes a collection owned by `user_id`.
    ///
    /// # Errors
    /// [`DavError::Invalid`] for a bad name or colour, [`DavError::Conflict`] when the slug is taken or at the limit.
    pub async fn create_collection(
        &self,
        user_id: Uuid,
        kind: Kind,
        slug: Option<&str>,
        display_name: &str,
        description: &str,
        color: Option<&str>,
    ) -> DavResult<Collection> {
        check_text(display_name, "name", 128, true)?;
        check_text(description, "description", 1000, false)?;
        if let Some(c) = color {
            check_color(c)?;
        }
        let owned = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM dav_collections WHERE owner_id = $1 AND kind = $2"#,
            user_id,
            kind.as_str()
        )
        .fetch_one(&self.pool)
        .await?;
        if owned >= MAX_COLLECTIONS {
            return Err(DavError::Conflict(format!(
                "at most {MAX_COLLECTIONS} per user"
            )));
        }
        let base = slug.map_or_else(|| slugify(display_name), str::to_owned);
        if !valid_slug(&base) {
            return Err(DavError::Invalid(
                "the name for the address is not valid".into(),
            ));
        }
        // Several collections may share a name; the URL part gets a number.
        let mut candidate = base.clone();
        for n in 2..50 {
            let taken = sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM dav_collections WHERE owner_id = $1 AND kind = $2 AND slug = $3"#, user_id, kind.as_str(), candidate).fetch_one(&self.pool).await?;
            if taken == 0 {
                break;
            }
            if slug.is_some() {
                return Err(DavError::Conflict("that address is already in use".into()));
            }
            candidate = format!("{base}-{n}");
        }
        let id = sqlx::query_scalar!(
            "INSERT INTO dav_collections (owner_id, kind, slug, display_name, description, color) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            user_id,
            kind.as_str(),
            candidate,
            display_name.trim(),
            description.trim(),
            color
        )
        .fetch_one(&self.pool)
        .await?;
        self.collection(user_id, id).await
    }

    /// Changes a collection's settings. Owner only.
    ///
    /// # Errors
    /// [`DavError::Forbidden`] for anyone but the owner; [`DavError::Invalid`] for bad values.
    pub async fn update_collection(
        &self,
        user_id: Uuid,
        id: Uuid,
        update: &CollectionUpdate,
    ) -> DavResult<()> {
        let c = self.collection(user_id, id).await?;
        if !c.access.is_owner() {
            return Err(DavError::Forbidden);
        }
        if let Some(n) = &update.display_name {
            check_text(n, "name", 128, true)?;
        }
        if let Some(d) = &update.description {
            check_text(d, "description", 1000, false)?;
        }
        if let Some(Some(col)) = &update.color {
            check_color(col)?;
        }
        let set_color = update.color.is_some();
        let color = update.color.clone().flatten();
        sqlx::query!(
            r#"UPDATE dav_collections SET
                 display_name = COALESCE($2, display_name),
                 description = COALESCE($3, description),
                 color = CASE WHEN $4 THEN $5 ELSE color END,
                 revision = revision + 1, updated_at = now()
               WHERE id = $1"#,
            id,
            update.display_name.as_deref().map(str::trim),
            update.description.as_deref().map(str::trim),
            set_color,
            color
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Deletes a collection and everything in it. Owner only.
    ///
    /// # Errors
    /// [`DavError::Forbidden`] for anyone but the owner.
    pub async fn delete_collection(&self, user_id: Uuid, id: Uuid) -> DavResult<()> {
        let c = self.collection(user_id, id).await?;
        if !c.access.is_owner() {
            return Err(DavError::Forbidden);
        }
        sqlx::query!("DELETE FROM dav_collections WHERE id = $1", id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ─── objects ─────────────────────────────────────────────────────────────

    async fn need(&self, user_id: Uuid, collection_id: Uuid, write: bool) -> DavResult<Collection> {
        let c = self.collection(user_id, collection_id).await?;
        if write && !c.access.can_write() {
            return Err(DavError::Forbidden);
        }
        Ok(c)
    }

    /// Lists a collection's objects without their text.
    ///
    /// # Errors
    /// [`DavError::NotFound`] without access.
    pub async fn list(&self, user_id: Uuid, collection_id: Uuid) -> DavResult<Vec<ObjectMeta>> {
        self.need(user_id, collection_id, false).await?;
        Ok(sqlx::query!(
            r#"SELECT name, etag, uid, component, summary, revision, length(data)::bigint AS "size!" FROM dav_objects WHERE collection_id = $1 ORDER BY name"#,
            collection_id
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|r| ObjectMeta { name: r.name, etag: r.etag, uid: r.uid, component: r.component, summary: r.summary, revision: r.revision, size: r.size })
        .collect())
    }

    /// One object with its text.
    ///
    /// # Errors
    /// [`DavError::NotFound`] if it or the collection is missing, or without access.
    pub async fn get(&self, user_id: Uuid, collection_id: Uuid, name: &str) -> DavResult<Object> {
        self.need(user_id, collection_id, false).await?;
        let r = sqlx::query!(
            r#"SELECT name, etag, uid, component, summary, revision, data, length(data)::bigint AS "size!" FROM dav_objects WHERE collection_id = $1 AND name = $2"#,
            collection_id,
            name
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(DavError::NotFound)?;
        Ok(Object {
            meta: ObjectMeta {
                name: r.name,
                etag: r.etag,
                uid: r.uid,
                component: r.component,
                summary: r.summary,
                revision: r.revision,
                size: r.size,
            },
            data: r.data,
        })
    }

    /// Objects by name, with their text. Missing names are left out.
    ///
    /// # Errors
    /// [`DavError::NotFound`] without access.
    pub async fn get_many(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        names: &[String],
    ) -> DavResult<Vec<Object>> {
        self.need(user_id, collection_id, false).await?;
        let rows = sqlx::query!(
            r#"SELECT name, etag, uid, component, summary, revision, data, length(data)::bigint AS "size!" FROM dav_objects WHERE collection_id = $1 AND name = ANY($2) ORDER BY name"#,
            collection_id,
            names
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Object {
                meta: ObjectMeta {
                    name: r.name,
                    etag: r.etag,
                    uid: r.uid,
                    component: r.component,
                    summary: r.summary,
                    revision: r.revision,
                    size: r.size,
                },
                data: r.data,
            })
            .collect())
    }

    /// Every object with its text (for a query that has to look at all of them).
    ///
    /// # Errors
    /// [`DavError::NotFound`] without access.
    pub async fn all(&self, user_id: Uuid, collection_id: Uuid) -> DavResult<Vec<Object>> {
        let names: Vec<String> = self
            .list(user_id, collection_id)
            .await?
            .into_iter()
            .map(|m| m.name)
            .collect();
        self.get_many(user_id, collection_id, &names).await
    }

    /// Events that may fall in `[from, to)`: a database pre-filter on the stored time extent.
    /// Callers still expand the result, because a repeating event's extent covers gaps.
    ///
    /// # Errors
    /// [`DavError::NotFound`] without access.
    pub async fn events_in_range(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> DavResult<Vec<Object>> {
        self.need(user_id, collection_id, false).await?;
        let rows = sqlx::query!(
            r#"SELECT name, etag, uid, component, summary, revision, data, length(data)::bigint AS "size!" FROM dav_objects
               WHERE collection_id = $1 AND component = 'VEVENT' AND starts_at < $3 AND (last_end IS NULL OR last_end >= $2)
               ORDER BY starts_at LIMIT 5000"#,
            collection_id,
            from,
            to
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Object {
                meta: ObjectMeta {
                    name: r.name,
                    etag: r.etag,
                    uid: r.uid,
                    component: r.component,
                    summary: r.summary,
                    revision: r.revision,
                    size: r.size,
                },
                data: r.data,
            })
            .collect())
    }

    /// Contacts whose name, organisation, email or phone contain `query` (case-insensitive).
    ///
    /// # Errors
    /// [`DavError::NotFound`] without access.
    pub async fn search_contacts(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        query: &str,
        limit: i64,
    ) -> DavResult<Vec<Object>> {
        self.need(user_id, collection_id, false).await?;
        // `%` and `_` are wildcards in LIKE; the user's text is matched literally.
        let escaped = query
            .trim()
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let rows = sqlx::query!(
            r#"SELECT name, etag, uid, component, summary, revision, data, length(data)::bigint AS "size!" FROM dav_objects
               WHERE collection_id = $1 AND search_text LIKE $2 ESCAPE '\'
               ORDER BY lower(summary), name LIMIT $3"#,
            collection_id,
            pattern,
            limit.clamp(1, 500)
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Object {
                meta: ObjectMeta {
                    name: r.name,
                    etag: r.etag,
                    uid: r.uid,
                    component: r.component,
                    summary: r.summary,
                    revision: r.revision,
                    size: r.size,
                },
                data: r.data,
            })
            .collect())
    }

    /// Stores an object, replacing one of the same name.
    ///
    /// # Errors
    /// [`DavError::Forbidden`] without write access; [`DavError::Invalid`] for bad data or name;
    /// [`DavError::PreconditionFailed`] when a condition does not hold;
    /// [`DavError::Conflict`] when the UID is used by another object or would change.
    pub async fn put(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        name: &str,
        data: &str,
        precondition: &Precondition,
    ) -> DavResult<Stored> {
        let c = self.need(user_id, collection_id, true).await?;
        if !valid_name(name) {
            return Err(DavError::Invalid("the object name is not valid".into()));
        }
        if data.len() > MAX_OBJECT_BYTES {
            return Err(DavError::Invalid("the object is too large".into()));
        }
        // (uid, component, summary, search, starts, ends, last_end, recurring, busy)
        let parsed = match c.kind {
            Kind::Calendar => {
                let i = calendar::parse_object(data)?;
                (
                    i.uid,
                    i.component.as_str(),
                    i.summary,
                    String::new(),
                    i.starts_at,
                    i.ends_at,
                    i.last_end,
                    i.recurring,
                    i.busy,
                )
            }
            Kind::AddressBook => {
                let i = contact::parse_contact(data)?;
                (
                    i.uid,
                    "VCARD",
                    i.display_name,
                    i.search_text,
                    None,
                    None,
                    None,
                    false,
                    false,
                )
            }
        };
        let (uid, component, summary, search, starts, ends, last_end, recurring, busy) = parsed;
        let etag = etag_of(data);
        let mut tx = self.pool.begin().await?;
        // Lock the collection so changes are numbered one after another.
        sqlx::query!(
            "SELECT id FROM dav_collections WHERE id = $1 FOR UPDATE",
            collection_id
        )
        .fetch_one(&mut *tx)
        .await?;
        let existing = sqlx::query!(
            "SELECT uid, etag FROM dav_objects WHERE collection_id = $1 AND name = $2",
            collection_id,
            name
        )
        .fetch_optional(&mut *tx)
        .await?;
        match (precondition, &existing) {
            (Precondition::Absent, Some(_)) => return Err(DavError::PreconditionFailed),
            (Precondition::Etag(want), Some(e)) if *want != e.etag => {
                return Err(DavError::PreconditionFailed)
            }
            (Precondition::Etag(_), None) => return Err(DavError::PreconditionFailed),
            _ => {}
        }
        if let Some(e) = &existing {
            if e.uid != uid {
                return Err(DavError::Conflict(
                    "the UID of an existing object cannot change".into(),
                ));
            }
            if e.etag == etag {
                return Ok(Stored {
                    etag,
                    created: false,
                });
            }
        } else {
            let count = sqlx::query_scalar!(
                r#"SELECT count(*) AS "n!" FROM dav_objects WHERE collection_id = $1"#,
                collection_id
            )
            .fetch_one(&mut *tx)
            .await?;
            if count >= MAX_OBJECTS {
                return Err(DavError::Conflict(format!(
                    "a collection holds at most {MAX_OBJECTS} items"
                )));
            }
        }
        if let Some(other) = sqlx::query_scalar!(
            "SELECT name FROM dav_objects WHERE collection_id = $1 AND uid = $2 AND name <> $3",
            collection_id,
            uid,
            name
        )
        .fetch_optional(&mut *tx)
        .await?
        {
            return Err(DavError::Conflict(format!(
                "the UID is already used by {other}"
            )));
        }
        let revision = sqlx::query_scalar!("UPDATE dav_collections SET revision = revision + 1, updated_at = now() WHERE id = $1 RETURNING revision", collection_id).fetch_one(&mut *tx).await?;
        sqlx::query!(
            r#"INSERT INTO dav_objects (collection_id, name, uid, etag, data, component, summary, search_text, starts_at, ends_at, last_end, recurring, busy, revision)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
               ON CONFLICT (collection_id, name) DO UPDATE SET
                 etag = $4, data = $5, component = $6, summary = $7, search_text = $8, starts_at = $9, ends_at = $10,
                 last_end = $11, recurring = $12, busy = $13, revision = $14, updated_at = now()"#,
            collection_id,
            name,
            uid,
            etag,
            data,
            component,
            summary,
            search,
            starts,
            ends,
            last_end,
            recurring,
            busy,
            revision
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "DELETE FROM dav_tombstones WHERE collection_id = $1 AND name = $2",
            collection_id,
            name
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Stored {
            etag,
            created: existing.is_none(),
        })
    }

    /// Removes an object.
    ///
    /// # Errors
    /// [`DavError::Forbidden`] without write access; [`DavError::NotFound`] if it is missing;
    /// [`DavError::PreconditionFailed`] if `if_match` is given and differs.
    pub async fn delete(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        name: &str,
        if_match: Option<&str>,
    ) -> DavResult<()> {
        self.need(user_id, collection_id, true).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "SELECT id FROM dav_collections WHERE id = $1 FOR UPDATE",
            collection_id
        )
        .fetch_one(&mut *tx)
        .await?;
        let existing = sqlx::query!(
            "SELECT etag FROM dav_objects WHERE collection_id = $1 AND name = $2",
            collection_id,
            name
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(DavError::NotFound)?;
        if if_match.is_some_and(|want| want != existing.etag) {
            return Err(DavError::PreconditionFailed);
        }
        let revision = sqlx::query_scalar!("UPDATE dav_collections SET revision = revision + 1, updated_at = now() WHERE id = $1 RETURNING revision", collection_id).fetch_one(&mut *tx).await?;
        sqlx::query!(
            "DELETE FROM dav_objects WHERE collection_id = $1 AND name = $2",
            collection_id,
            name
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!(
            "INSERT INTO dav_tombstones (collection_id, name, revision) VALUES ($1, $2, $3)",
            collection_id,
            name,
            revision
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// What changed since `since` (0 for everything).
    ///
    /// # Errors
    /// [`DavError::Invalid`] if `since` is from the future (a token this collection never issued).
    pub async fn changes_since(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        since: i64,
    ) -> DavResult<Changes> {
        let c = self.need(user_id, collection_id, false).await?;
        if since < 0 || since > c.revision {
            return Err(DavError::Invalid("unknown sync token".into()));
        }
        let changed = sqlx::query!(
            r#"SELECT name, etag, uid, component, summary, revision, length(data)::bigint AS "size!" FROM dav_objects WHERE collection_id = $1 AND revision > $2 ORDER BY revision, name"#,
            collection_id,
            since
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|r| ObjectMeta { name: r.name, etag: r.etag, uid: r.uid, component: r.component, summary: r.summary, revision: r.revision, size: r.size })
        .collect();
        let deleted = if since == 0 {
            Vec::new()
        } else {
            sqlx::query_scalar!("SELECT DISTINCT name FROM dav_tombstones WHERE collection_id = $1 AND revision > $2 ORDER BY name", collection_id, since).fetch_all(&self.pool).await?
        };
        Ok(Changes {
            revision: c.revision,
            changed,
            deleted,
        })
    }

    // ─── sharing ─────────────────────────────────────────────────────────────

    /// Who a collection is shared with. Owner only.
    ///
    /// # Errors
    /// [`DavError::Forbidden`] for anyone but the owner.
    pub async fn shares(&self, user_id: Uuid, collection_id: Uuid) -> DavResult<Vec<Share>> {
        let c = self.collection(user_id, collection_id).await?;
        if !c.access.is_owner() {
            return Err(DavError::Forbidden);
        }
        Ok(sqlx::query!(
            r#"SELECT s.id, s.can_write, s.grantee_user_id, u.email AS "email?", s.grantee_group_id, g.name AS "group_name?"
               FROM dav_shares s LEFT JOIN users u ON u.id = s.grantee_user_id LEFT JOIN groups g ON g.id = s.grantee_group_id
               WHERE s.collection_id = $1 ORDER BY lower(COALESCE(u.email, g.name))"#,
            collection_id
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .filter_map(|r| {
            let grantee = match (r.grantee_user_id, r.email, r.grantee_group_id, r.group_name) {
                (Some(id), Some(email), _, _) => Grantee::User { id, email },
                (_, _, Some(id), Some(name)) => Grantee::Group { id, name },
                _ => return None,
            };
            Some(Share { id: r.id, grantee, can_write: r.can_write })
        })
        .collect())
    }

    /// Shares a collection, or changes an existing share's right. Owner only. Sharing stays
    /// inside the owner's domain.
    ///
    /// # Errors
    /// [`DavError::Forbidden`] for anyone but the owner; [`DavError::Invalid`] for an unknown
    /// grantee, one in another domain, or the owner themselves.
    pub async fn share(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        with: &GranteeRef,
        can_write: bool,
    ) -> DavResult<()> {
        let c = self.collection(user_id, collection_id).await?;
        if !c.access.is_owner() {
            return Err(DavError::Forbidden);
        }
        let owner_domain =
            sqlx::query_scalar!("SELECT domain_id FROM users WHERE id = $1", c.owner_id)
                .fetch_one(&self.pool)
                .await?;
        match with {
            GranteeRef::UserEmail(email) => {
                let target = sqlx::query!(r#"SELECT id, domain_id FROM users WHERE lower(email) = lower($1) AND status = 'active'"#, email.trim()).fetch_optional(&self.pool).await?;
                let Some(t) = target else {
                    return Err(DavError::Invalid(
                        "there is no active user with that address".into(),
                    ));
                };
                if t.id == c.owner_id {
                    return Err(DavError::Invalid("a calendar is already yours".into()));
                }
                if t.domain_id != owner_domain {
                    return Err(DavError::Invalid(
                        "you can only share with people in your own domain".into(),
                    ));
                }
                sqlx::query!(
                    "INSERT INTO dav_shares (collection_id, grantee_user_id, can_write) VALUES ($1, $2, $3) ON CONFLICT (collection_id, grantee_user_id) WHERE grantee_user_id IS NOT NULL DO UPDATE SET can_write = $3",
                    collection_id,
                    t.id,
                    can_write
                )
                .execute(&self.pool)
                .await?;
            }
            GranteeRef::Group(group_id) => {
                let group_domain =
                    sqlx::query_scalar!("SELECT domain_id FROM groups WHERE id = $1", group_id)
                        .fetch_optional(&self.pool)
                        .await?;
                match group_domain {
                    Some(d) if d == owner_domain => {}
                    Some(_) => {
                        return Err(DavError::Invalid(
                            "you can only share with groups in your own domain".into(),
                        ))
                    }
                    None => return Err(DavError::Invalid("there is no such group".into())),
                }
                sqlx::query!(
                    "INSERT INTO dav_shares (collection_id, grantee_group_id, can_write) VALUES ($1, $2, $3) ON CONFLICT (collection_id, grantee_group_id) WHERE grantee_group_id IS NOT NULL DO UPDATE SET can_write = $3",
                    collection_id,
                    group_id,
                    can_write
                )
                .execute(&self.pool)
                .await?;
            }
        }
        // Clients notice a change in who may see a collection through its revision.
        sqlx::query!(
            "UPDATE dav_collections SET revision = revision + 1 WHERE id = $1",
            collection_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Stops sharing. Owner only.
    ///
    /// # Errors
    /// [`DavError::Forbidden`] for anyone but the owner; [`DavError::NotFound`] for an unknown share.
    pub async fn unshare(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        share_id: Uuid,
    ) -> DavResult<()> {
        let c = self.collection(user_id, collection_id).await?;
        if !c.access.is_owner() {
            return Err(DavError::Forbidden);
        }
        let n = sqlx::query!(
            "DELETE FROM dav_shares WHERE id = $1 AND collection_id = $2",
            share_id,
            collection_id
        )
        .execute(&self.pool)
        .await?
        .rows_affected();
        if n == 0 {
            return Err(DavError::NotFound);
        }
        sqlx::query!(
            "UPDATE dav_collections SET revision = revision + 1 WHERE id = $1",
            collection_id
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The groups in the user's domain, for the share form.
    ///
    /// # Errors
    /// Database errors.
    pub async fn groups_for(&self, user_id: Uuid) -> DavResult<Vec<(Uuid, String)>> {
        Ok(sqlx::query!("SELECT g.id, g.name FROM groups g JOIN users u ON u.domain_id = g.domain_id WHERE u.id = $1 ORDER BY lower(g.name)", user_id).fetch_all(&self.pool).await?.into_iter().map(|r| (r.id, r.name)).collect())
    }

    // ─── what the calendar page shows ────────────────────────────────────────

    /// Every occurrence in `[from, to)` across the user's own and shared calendars.
    ///
    /// # Errors
    /// [`DavError::Invalid`] for a window longer than [`MAX_WINDOW_DAYS`].
    pub async fn calendar_events(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> DavResult<Vec<CalendarEvent>> {
        check_window(from, to)?;
        let mut out = Vec::new();
        for c in self.collections(user_id, Kind::Calendar).await? {
            for object in self.events_in_range(user_id, c.id, from, to).await? {
                match calendar::occurrences(&object.data, from, to) {
                    Ok(list) => out.extend(list.into_iter().map(|occurrence| CalendarEvent {
                        collection_id: c.id,
                        color: c.color.clone(),
                        object: object.meta.name.clone(),
                        writable: c.access.can_write(),
                        occurrence,
                    })),
                    Err(error) => {
                        tracing::warn!(%error, object = %object.meta.name, "skipping an unreadable calendar object")
                    }
                }
            }
            if out.len() > 5000 {
                break;
            }
        }
        out.sort_by_key(|e| (e.occurrence.start, e.occurrence.end));
        out.truncate(5000);
        Ok(out)
    }

    /// When `target_email` is busy in `[from, to)`: times only, no titles. The asker must be
    /// in the same domain as the person they ask about. Only the person's own calendars count,
    /// and only events that block time (not free, not cancelled).
    ///
    /// # Errors
    /// [`DavError::NotFound`] for an unknown person or another domain; [`DavError::Invalid`] for a bad window.
    pub async fn free_busy(
        &self,
        asker_id: Uuid,
        target_email: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> DavResult<Vec<Busy>> {
        check_window(from, to)?;
        let target = sqlx::query_scalar!(
            r#"SELECT t.id FROM users t JOIN users a ON a.domain_id = t.domain_id
               WHERE lower(t.email) = lower($1) AND a.id = $2 AND t.status = 'active'"#,
            target_email.trim(),
            asker_id
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(DavError::NotFound)?;
        let rows = sqlx::query_scalar!(
            r#"SELECT o.data FROM dav_objects o JOIN dav_collections c ON c.id = o.collection_id
               WHERE c.owner_id = $1 AND c.kind = 'calendar' AND o.component = 'VEVENT' AND o.busy
                 AND o.starts_at < $3 AND (o.last_end IS NULL OR o.last_end >= $2)
               LIMIT 5000"#,
            target,
            from,
            to
        )
        .fetch_all(&self.pool)
        .await?;
        let mut periods = Vec::new();
        for data in rows {
            if let Ok(list) = calendar::occurrences(&data, from, to) {
                periods.extend(list.into_iter().filter(|o| o.busy).map(|o| {
                    Busy {
                        start: o.start.max(from),
                        end: o
                            .end
                            .min(to)
                            .max(o.start.max(from) + Duration::minutes(1))
                            .min(to),
                    }
                }));
            }
        }
        Ok(merge(periods))
    }
}

fn check_window(from: DateTime<Utc>, to: DateTime<Utc>) -> DavResult<()> {
    if to <= from || to - from > Duration::days(MAX_WINDOW_DAYS) {
        return Err(DavError::Invalid(format!(
            "choose a period of at most {MAX_WINDOW_DAYS} days"
        )));
    }
    Ok(())
}

/// Sorts periods and joins those that overlap or touch.
#[must_use]
pub fn merge(mut periods: Vec<Busy>) -> Vec<Busy> {
    periods.retain(|p| p.end > p.start);
    periods.sort_by_key(|p| p.start);
    let mut out: Vec<Busy> = Vec::new();
    for p in periods {
        match out.last_mut() {
            Some(last) if p.start <= last.end => last.end = last.end.max(p.end),
            _ => out.push(p),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2025-03-10T{h:02}:{m:02}:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn busy_periods_are_merged() {
        let merged = merge(vec![
            Busy {
                start: at(13, 0),
                end: at(14, 0),
            },
            Busy {
                start: at(9, 0),
                end: at(10, 0),
            },
            Busy {
                start: at(9, 30),
                end: at(11, 0),
            },
            Busy {
                start: at(11, 0),
                end: at(11, 30),
            },
            Busy {
                start: at(15, 0),
                end: at(15, 0),
            },
        ]);
        assert_eq!(
            merged,
            vec![
                Busy {
                    start: at(9, 0),
                    end: at(11, 30)
                },
                Busy {
                    start: at(13, 0),
                    end: at(14, 0)
                }
            ]
        );
    }

    #[test]
    fn names_and_slugs_are_strict() {
        for ok in ["a.ics", "5f3a-7.vcf", "x@y=z+1_2.ics"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".hidden",
            "-x",
            "a/b",
            "a b",
            "a\n.ics",
            "../x",
            "é.ics",
            &"a".repeat(202),
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_slug("work-2") && valid_slug("a.b_c"));
        for bad in ["", "-a", "A", "a b", "a/b", &"a".repeat(64)] {
            assert!(!valid_slug(bad), "{bad:?}");
        }
        assert_eq!(slugify("  Team Calendar! "), "team-calendar");
        assert_eq!(slugify("日本語"), "collection");
        assert!(valid_slug(&slugify("Ünïcode & Co. (2025)")));
    }

    #[test]
    fn etags_follow_the_text() {
        assert_eq!(etag_of("a"), etag_of("a"));
        assert_ne!(etag_of("a"), etag_of("b"));
        assert_eq!(etag_of("a").len(), 32);
    }

    #[test]
    fn access_levels_are_ordered() {
        assert!(
            Access::Owner.can_write() && Access::ReadWrite.can_write() && !Access::Read.can_write()
        );
        assert!(Access::Owner.is_owner() && !Access::ReadWrite.is_owner());
        assert_eq!(Access::from_level(0), None);
        assert!(Access::Owner > Access::ReadWrite && Access::ReadWrite > Access::Read);
    }

    #[test]
    fn windows_are_bounded() {
        assert!(check_window(at(0, 0), at(1, 0)).is_ok());
        assert!(check_window(at(1, 0), at(0, 0)).is_err());
        assert!(check_window(at(0, 0), at(0, 0) + Duration::days(401)).is_err());
    }
}
