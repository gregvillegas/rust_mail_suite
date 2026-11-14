//! Domain model: plain data types mirroring the database schema.
//!
//! Types derive [`serde`] traits and [`sqlx::FromRow`]. Secrets (password and
//! token hashes) are excluded from serialisation.

pub mod accounts;
pub mod enums;
pub mod groupware;
pub mod routing;
pub mod rules;
pub mod security;
pub mod storage;
pub mod validation;

pub use accounts::{LoginFailure, MailDomain, Mailbox, NewDomain, NewMailbox, NewUser, User};
pub use enums::{AccountStatus, AuditOutcome, UserRole};
pub use groupware::{CalendarEvent, Contact};
pub use routing::{Alias, Group, NewAlias, NewGroup};
pub use rules::{Autoresponder, MailFilter, SieveScript};
pub use security::{AuditEntry, AuditQuery, NewAuditEntry, NewSession, Session};
pub use storage::{MessageMeta, QuotaUsage};
