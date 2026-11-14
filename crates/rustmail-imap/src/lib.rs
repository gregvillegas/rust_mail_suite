//! # rustmail-imap
//!
//! The webmail's mail client library (Phase 6): everything that talks to the mail
//! server on a user's behalf.
//!
//! * [`config`] and [`tls`]: where the servers are and how to reach them securely.
//! * [`session`]: an IMAP session for one user (folders, messages, flags, moves),
//!   opened with Dovecot's *master user* feature so the web server never needs
//!   the user's password.
//! * [`message`]: parsing a raw message for display, sanitising HTML, extracting
//!   attachments.
//! * [`compose`]: building an outgoing message.
//! * [`submit`]: sending it through Postfix's submission port.
//! * [`sync`]: filling the search index from IMAP.
//! * [`utf7`]: IMAP's modified UTF-7 folder-name encoding.
//!
//! Nothing here renders HTML pages or knows about HTTP; that is `rustmail-web`.
#![deny(warnings)]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod compose;
pub mod config;
pub mod error;
pub mod folder;
pub mod message;
pub mod session;
pub mod submit;
pub mod sync;
pub mod tls;
pub mod utf7;

pub use config::{MailConfig, TlsMode};
pub use error::{MailError, MailResult};
pub use session::MailSession;
