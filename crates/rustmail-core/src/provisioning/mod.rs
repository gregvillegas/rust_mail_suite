//! Mailbox provisioning: creating Maildirs on disk and keeping them consistent
//! with the database.
//!
//! - [`MailboxProvisioner`] makes the directories. It never follows symlinks,
//!   rejects any path that could leave the mail base directory, sets exact
//!   permissions, and is idempotent.
//! - [`AccountService`] creates domains and users together with their Maildirs,
//!   undoing the database change if the disk step fails.
//!
//! ## Permissions model
//!
//! Dovecot (running as the `vmail` user) owns mail delivery and IMAP. The
//! provisioner runs as whichever user runs RustMail and never calls `chown`, so
//! it needs no privileges. The two cooperate through filesystem permissions:
//!
//! - **Same user:** run RustMail as `vmail`; the default mode `0700` is right.
//! - **Different users (recommended for production):** make the base directory
//!   `vmail:vmail` with mode `2770` (setgid), put the RustMail user in group
//!   `vmail`, and set `VMAIL_DIR_MODE=0770`. New directories inherit the group
//!   and are usable by Dovecot.

mod service;

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub use service::{AccountService, CheckReport, CreateUser, CreatedUser, MailboxProblem};

/// Where mail lives unless configured otherwise.
pub const DEFAULT_BASE_DIR: &str = "/var/mail/vhosts";

/// Name of the Maildir directory inside a user's home.
const MAILDIR: &str = "Maildir";
/// The three subdirectories every Maildir has.
const MAILDIR_SUBDIRS: [&str; 3] = ["cur", "new", "tmp"];
const MAX_COMPONENT_BYTES: usize = 255;
const MAX_PATH_BYTES: usize = 1024;

/// Settings for [`MailboxProvisioner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionerConfig {
    /// The mail base directory (`vmail_base` in the Dovecot config). Must exist.
    pub base_dir: PathBuf,
    /// Permission bits for directories the provisioner creates.
    pub dir_mode: u32,
}

impl ProvisionerConfig {
    /// A config with the private default mode `0700`.
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            dir_mode: 0o700,
        }
    }

    /// Sets the directory mode.
    ///
    /// # Errors
    /// Returns [`Error::Config`] unless the mode gives the owner full access
    /// (Dovecot must be able to use the directory) and sets no special bits.
    pub fn with_dir_mode(mut self, mode: u32) -> Result<Self> {
        if mode & !0o777 != 0 || mode & 0o700 != 0o700 {
            return Err(Error::Config(format!(
                "directory mode {mode:04o} must be of the form 07xx without special bits"
            )));
        }
        self.dir_mode = mode;
        Ok(self)
    }

    /// Builds a config from optional settings, applying the defaults
    /// ([`DEFAULT_BASE_DIR`], mode `0700`).
    ///
    /// # Errors
    /// Returns [`Error::Config`] for an unparsable or unsafe mode.
    pub fn from_parts(base_dir: Option<PathBuf>, dir_mode: Option<&str>) -> Result<Self> {
        let config = Self::new(base_dir.unwrap_or_else(|| PathBuf::from(DEFAULT_BASE_DIR)));
        match dir_mode {
            None => Ok(config),
            Some(raw) => {
                let mode =
                    u32::from_str_radix(raw.trim().trim_start_matches("0o"), 8).map_err(|_| {
                        Error::Config(format!("VMAIL_DIR_MODE must be octal, got '{raw}'"))
                    })?;
                config.with_dir_mode(mode)
            }
        }
    }

    /// Loads `VMAIL_BASE` (default [`DEFAULT_BASE_DIR`]) and `VMAIL_DIR_MODE`
    /// (octal, default `0700`) from the environment.
    ///
    /// # Errors
    /// Returns [`Error::Config`] for an unparsable or unsafe mode.
    pub fn from_env() -> Result<Self> {
        let base = std::env::var_os("VMAIL_BASE").map(PathBuf::from);
        let mode = std::env::var("VMAIL_DIR_MODE").ok();
        Self::from_parts(base, mode.as_deref())
    }
}

/// What a provisioning call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionOutcome {
    /// The directory that was ensured.
    pub path: PathBuf,
    /// Whether anything had to be created (false when it was already complete).
    pub created: bool,
}

/// Creates Maildirs on disk.
#[derive(Debug, Clone)]
pub struct MailboxProvisioner {
    config: ProvisionerConfig,
}

impl MailboxProvisioner {
    /// Creates a provisioner.
    #[must_use]
    pub fn new(config: ProvisionerConfig) -> Self {
        Self { config }
    }

    /// The mail base directory.
    #[must_use]
    pub fn base_dir(&self) -> &Path {
        &self.config.base_dir
    }

    /// The database form of a user's home, `domain/local/`, relative to the
    /// base directory. Inputs must already be normalised (see
    /// [`crate::domain::validation`]).
    #[must_use]
    pub fn home_for(domain: &str, local_part: &str) -> String {
        format!("{domain}/{local_part}/")
    }

    /// Ensures the per-domain directory exists.
    ///
    /// # Errors
    /// Returns [`Error::Provisioning`] if the base directory is unusable or the
    /// directory cannot be created.
    pub fn ensure_domain_dir(&self, domain: &str) -> Result<ProvisionOutcome> {
        let components = safe_components(domain)?;
        if components.len() != 1 {
            return Err(Error::Provisioning(format!(
                "'{domain}' is not a single directory name"
            )));
        }
        let base = self.checked_base()?;
        let (path, created) = self.ensure_chain(&base, &components)?;
        Ok(ProvisionOutcome { path, created })
    }

    /// Ensures `<base>/<home>/Maildir/{cur,new,tmp}` exists, where `home` is a
    /// mailbox's `maildir_path` from the database.
    ///
    /// # Errors
    /// Returns [`Error::Provisioning`] if `home` could escape the base directory,
    /// a path component is a symlink or a file, or the filesystem refuses.
    pub fn ensure_maildir(&self, home: &str) -> Result<ProvisionOutcome> {
        let mut components = safe_components(home)?;
        components.push(MAILDIR);
        let base = self.checked_base()?;
        let (maildir, mut created) = self.ensure_chain(&base, &components)?;
        for sub in MAILDIR_SUBDIRS {
            let (_, made) = self.ensure_chain(&maildir, &[sub])?;
            created |= made;
        }
        Ok(ProvisionOutcome {
            path: maildir,
            created,
        })
    }

    /// Removes a user's whole home directory (`<domain>/<local>/`): the Maildir,
    /// Sieve scripts and everything else in it. Returns whether anything was removed.
    ///
    /// This is irreversible, so it is strict: `home` must be exactly two safe path
    /// components, the directory must be a real directory (not a symlink) inside the
    /// mail base, and symlinks inside it are removed, never followed.
    ///
    /// # Errors
    /// [`Error::Provisioning`] if the path is unsafe or removal fails.
    pub fn remove_home(&self, home: &str) -> Result<bool> {
        let components = safe_components(home)?;
        if components.len() != 2 {
            return Err(Error::Provisioning(format!(
                "'{home}' is not a <domain>/<user> directory; refusing to remove it"
            )));
        }
        let base = self.checked_base()?;
        let mut path = base.clone();
        for component in &components {
            path.push(component);
        }
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                Err(Error::Provisioning(format!(
                    "{} is not a plain directory; refusing to remove it",
                    path.display()
                )))
            }
            Ok(_) => {
                let real = fs::canonicalize(&path).map_err(|e| io_error(&path, &e))?;
                let real_base = fs::canonicalize(&base).map_err(|e| io_error(&base, &e))?;
                if !real.starts_with(&real_base) || real == real_base {
                    return Err(Error::Provisioning(format!(
                        "{} is outside the mail base",
                        path.display()
                    )));
                }
                fs::remove_dir_all(&real).map_err(|e| io_error(&real, &e))?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_error(&path, &e)),
        }
    }

    /// Whether the full Maildir skeleton for `home` exists. Never creates anything.
    ///
    /// # Errors
    /// Returns [`Error::Provisioning`] only if `home` is an unsafe path.
    pub fn maildir_exists(&self, home: &str) -> Result<bool> {
        let mut path = self.config.base_dir.clone();
        for component in safe_components(home)? {
            path.push(component);
        }
        path.push(MAILDIR);
        Ok(MAILDIR_SUBDIRS
            .iter()
            .all(|sub| is_real_dir(&path.join(sub))))
    }

    /// The base directory, verified to be an existing real directory.
    fn checked_base(&self) -> Result<PathBuf> {
        let base = &self.config.base_dir;
        match fs::metadata(base) {
            Ok(meta) if meta.is_dir() => Ok(base.clone()),
            Ok(_) => Err(Error::Provisioning(format!("mail base {} is not a directory", base.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Error::Provisioning(format!(
                "mail base directory {} does not exist; create it (see scripts/setup-mta.sh) or set VMAIL_BASE",
                base.display()
            ))),
            Err(e) => Err(Error::Provisioning(format!("cannot access mail base {}: {e}", base.display()))),
        }
    }

    /// Creates each component beneath `start`, refusing symlinks and non-directories.
    /// Returns the final path and whether anything was created.
    fn ensure_chain(&self, start: &Path, components: &[&str]) -> Result<(PathBuf, bool)> {
        let mut path = start.to_path_buf();
        let mut created = false;
        for component in components {
            path.push(component);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(Error::Provisioning(format!(
                        "{} is a symlink; refusing to follow it",
                        path.display()
                    )));
                }
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(Error::Provisioning(format!(
                        "{} exists and is not a directory",
                        path.display()
                    )));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    created |= self.make_dir(&path)?;
                }
                Err(e) => return Err(io_error(&path, &e)),
            }
        }
        Ok((path, created))
    }

    /// Creates one directory with exactly `dir_mode`. Returns false if another
    /// process created it first.
    fn make_dir(&self, path: &Path) -> Result<bool> {
        match fs::DirBuilder::new()
            .mode(self.config.dir_mode)
            .create(path)
        {
            Ok(()) => {
                // `mode` above is filtered through the umask (022 would turn the
                // group-writable 0770 into 0750), so state the mode explicitly.
                fs::set_permissions(path, fs::Permissions::from_mode(self.config.dir_mode))
                    .map_err(|e| io_error(path, &e))?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(io_error(path, &e)),
        }
    }
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

fn io_error(path: &Path, error: &io::Error) -> Error {
    Error::Provisioning(format!("{}: {error}", path.display()))
}

/// Splits a relative path into components, rejecting anything that could leave
/// the base directory or confuse the filesystem.
fn safe_components(relative: &str) -> Result<Vec<&str>> {
    let bad = |why: &str| {
        Error::Provisioning(format!(
            "unsafe mailbox path '{}': {why}",
            relative.escape_debug()
        ))
    };
    if relative.is_empty() || relative.len() > MAX_PATH_BYTES {
        return Err(bad("empty or too long"));
    }
    if relative.starts_with('/') {
        return Err(bad("must be relative"));
    }
    let mut components = Vec::new();
    for component in relative.split('/') {
        if component.is_empty() {
            continue; // trailing or doubled slash
        }
        if component == "." || component == ".." {
            return Err(bad("'.' and '..' are not allowed"));
        }
        if component.len() > MAX_COMPONENT_BYTES {
            return Err(bad("component too long"));
        }
        if component
            .chars()
            .any(|c| c == '\0' || c == '\\' || c.is_control())
        {
            return Err(bad("control characters and backslashes are not allowed"));
        }
        components.push(component);
    }
    if components.is_empty() {
        return Err(bad("no components"));
    }
    Ok(components)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A scratch directory removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "rustmail-prov-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }

        fn provisioner(&self, mode: u32) -> MailboxProvisioner {
            MailboxProvisioner::new(
                ProvisionerConfig::new(&self.0)
                    .with_dir_mode(mode)
                    .expect("mode"),
            )
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
    }

    #[test]
    fn creates_the_full_maildir_skeleton() {
        let scratch = Scratch::new();
        let outcome = scratch
            .provisioner(0o700)
            .ensure_maildir("example.com/alice/")
            .expect("provision");

        assert!(outcome.created);
        assert_eq!(outcome.path, scratch.0.join("example.com/alice/Maildir"));
        for sub in ["cur", "new", "tmp"] {
            assert!(outcome.path.join(sub).is_dir(), "{sub}");
        }
    }

    #[test]
    fn is_idempotent_and_can_complete_a_partial_maildir() {
        let scratch = Scratch::new();
        let p = scratch.provisioner(0o700);
        assert!(
            p.ensure_maildir("example.com/alice/")
                .expect("first")
                .created
        );
        assert!(
            !p.ensure_maildir("example.com/alice/")
                .expect("second")
                .created
        );

        fs::remove_dir(scratch.0.join("example.com/alice/Maildir/tmp")).expect("remove tmp");
        assert!(!p.maildir_exists("example.com/alice/").expect("check"));
        assert!(
            p.ensure_maildir("example.com/alice/")
                .expect("repair")
                .created
        );
        assert!(p.maildir_exists("example.com/alice/").expect("check"));
    }

    #[test]
    fn modes_are_exact_despite_the_umask() {
        let scratch = Scratch::new();
        // 0770 would become 0750 under a 022 umask if it were not set explicitly.
        let outcome = scratch
            .provisioner(0o770)
            .ensure_maildir("example.com/alice/")
            .expect("provision");
        for dir in [
            scratch.0.join("example.com"),
            scratch.0.join("example.com/alice"),
            outcome.path.clone(),
            outcome.path.join("cur"),
        ] {
            assert_eq!(mode_of(&dir), 0o770, "{}", dir.display());
        }
        let private = Scratch::new();
        private
            .provisioner(0o700)
            .ensure_maildir("example.com/bob/")
            .expect("provision");
        assert_eq!(mode_of(&private.0.join("example.com/bob/Maildir")), 0o700);
    }

    #[test]
    fn never_touches_the_base_directory_itself() {
        let scratch = Scratch::new();
        fs::set_permissions(&scratch.0, fs::Permissions::from_mode(0o751)).expect("chmod");
        scratch
            .provisioner(0o700)
            .ensure_maildir("example.com/alice/")
            .expect("provision");
        assert_eq!(mode_of(&scratch.0), 0o751);
    }

    #[test]
    fn rejects_paths_that_could_escape_the_base() {
        let scratch = Scratch::new();
        let p = scratch.provisioner(0o700);
        for bad in [
            "",
            "/",
            "/etc/passwd",
            "../outside",
            "a/../../outside",
            "a/./b",
            "..",
            ".",
            "a/..",
            "a\\b",
            "a\0b",
            "a/\nb",
            "a/\tb",
        ] {
            assert!(
                matches!(p.ensure_maildir(bad), Err(Error::Provisioning(_))),
                "{bad:?} must be rejected"
            );
            assert!(
                matches!(p.maildir_exists(bad), Err(Error::Provisioning(_))),
                "{bad:?} must be rejected"
            );
        }
        assert!(
            p.ensure_maildir(&format!("{}/x", "a".repeat(256))).is_err(),
            "over-long component"
        );
        // Nothing was created anywhere, including next to the scratch directory.
        assert_eq!(fs::read_dir(&scratch.0).expect("list").count(), 0);
        assert!(!scratch.0.parent().expect("parent").join("outside").exists());
    }

    #[test]
    fn doubled_and_trailing_slashes_are_harmless() {
        let scratch = Scratch::new();
        let outcome = scratch
            .provisioner(0o700)
            .ensure_maildir("example.com//alice//")
            .expect("provision");
        assert_eq!(outcome.path, scratch.0.join("example.com/alice/Maildir"));
    }

    #[test]
    fn refuses_to_follow_a_symlink_out_of_the_base() {
        let scratch = Scratch::new();
        let outside = Scratch::new();
        symlink(&outside.0, scratch.0.join("example.com")).expect("symlink");

        let result = scratch
            .provisioner(0o700)
            .ensure_maildir("example.com/alice/");
        assert!(
            matches!(&result, Err(Error::Provisioning(m)) if m.contains("symlink")),
            "{result:?}"
        );
        assert_eq!(
            fs::read_dir(&outside.0).expect("list").count(),
            0,
            "nothing may be created through the link"
        );
        assert!(!scratch
            .provisioner(0o700)
            .maildir_exists("example.com/alice/")
            .expect("check"));
    }

    #[test]
    fn refuses_a_symlinked_maildir_or_a_file_in_the_way() {
        let scratch = Scratch::new();
        let outside = Scratch::new();
        let p = scratch.provisioner(0o700);
        fs::create_dir_all(scratch.0.join("example.com/alice")).expect("home");
        symlink(&outside.0, scratch.0.join("example.com/alice/Maildir")).expect("symlink");
        assert!(p.ensure_maildir("example.com/alice/").is_err());

        fs::write(scratch.0.join("example.com/bob"), b"not a directory").expect("file");
        let result = p.ensure_maildir("example.com/bob/");
        assert!(
            matches!(&result, Err(Error::Provisioning(m)) if m.contains("not a directory")),
            "{result:?}"
        );
    }

    #[test]
    fn a_missing_base_directory_is_an_actionable_error() {
        let provisioner =
            MailboxProvisioner::new(ProvisionerConfig::new("/nonexistent/rustmail/base"));
        let result = provisioner.ensure_maildir("example.com/alice/");
        assert!(
            matches!(&result, Err(Error::Provisioning(m)) if m.contains("does not exist") && m.contains("VMAIL_BASE"))
        );
    }

    #[test]
    fn domain_directories() {
        let scratch = Scratch::new();
        let p = scratch.provisioner(0o700);
        assert!(p.ensure_domain_dir("example.com").expect("create").created);
        assert!(!p.ensure_domain_dir("example.com").expect("again").created);
        assert!(
            p.ensure_domain_dir("a/b").is_err(),
            "a domain is exactly one component"
        );
        assert!(p.ensure_domain_dir("..").is_err());
    }

    #[test]
    fn home_paths_match_what_the_database_stores() {
        assert_eq!(
            MailboxProvisioner::home_for("example.com", "alice"),
            "example.com/alice/"
        );
    }

    #[test]
    fn config_parts_apply_defaults_and_parse_octal() {
        let default = ProvisionerConfig::from_parts(None, None).expect("defaults");
        assert_eq!(
            (default.base_dir.as_path(), default.dir_mode),
            (Path::new(DEFAULT_BASE_DIR), 0o700)
        );
        let custom = ProvisionerConfig::from_parts(Some("/srv/mail".into()), Some(" 0770 "))
            .expect("custom");
        assert_eq!(
            (custom.base_dir.as_path(), custom.dir_mode),
            (Path::new("/srv/mail"), 0o770)
        );
        assert_eq!(
            ProvisionerConfig::from_parts(None, Some("0o750"))
                .expect("0o prefix")
                .dir_mode,
            0o750
        );
        for bad in ["rwx", "9", "", "0600", "4755"] {
            assert!(
                matches!(
                    ProvisionerConfig::from_parts(None, Some(bad)),
                    Err(Error::Config(_))
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn directory_modes_are_validated() {
        let base = ProvisionerConfig::new("/x");
        assert!(base.clone().with_dir_mode(0o700).is_ok());
        assert!(base.clone().with_dir_mode(0o770).is_ok());
        assert!(base.clone().with_dir_mode(0o777).is_ok());
        for bad in [0o000, 0o600, 0o500, 0o2770, 0o4700, 0o1777, 0o7777, 0o10000] {
            assert!(base.clone().with_dir_mode(bad).is_err(), "{bad:o}");
        }
    }
}
