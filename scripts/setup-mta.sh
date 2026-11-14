#!/usr/bin/env bash
# Provision Postfix + Dovecot for RustMail Suite on Debian 12 (Dovecot 2.3).
#
# Idempotent: running it again with unchanged inputs changes nothing and reloads
# nothing. Configuration files are rendered from deploy/**/*.j2, validated, and
# rolled back from a backup if validation fails.
#
#   sudo scripts/setup-mta.sh --env-file /etc/rustmail/mta.env
#
# Flags:
#   --env-file FILE      settings (default: /etc/rustmail/mta.env; see deploy/mta.env.example)
#   --packages-only      install packages and stop
#   --skip-packages      do not install packages
#   --skip-db            do not create/update the database roles
#   --no-reload          do not reload services after a change
#   --container          dev container: no service reloads, tolerate bind-mount ownership limits
#   --check              change nothing; exit 1 if the installed files differ from the templates
#   --render-to DIR      only render every template into DIR (used by scripts/test-templates.sh)
#   -h, --help
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE=/etc/rustmail/mta.env
PACKAGES=1 PACKAGES_ONLY=0 DB=1 RELOAD=1 CONTAINER=0 CHECK=0 RENDER_TO=""

log() { printf '[setup-mta] %s\n' "$*" >&2; }
die() { printf '[setup-mta] ERROR: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --env-file) [ $# -ge 2 ] || die "--env-file needs a value"; ENV_FILE="$2"; shift ;;
    --packages-only) PACKAGES_ONLY=1 ;;
    --skip-packages) PACKAGES=0 ;;
    --skip-db) DB=0 ;;
    --no-reload) RELOAD=0 ;;
    --container) CONTAINER=1; RELOAD=0 ;;
    --check) CHECK=1; PACKAGES=0; DB=0; RELOAD=0 ;;
    --render-to) [ $# -ge 2 ] || die "--render-to needs a value"; RENDER_TO="$2"; PACKAGES=0; DB=0; RELOAD=0; shift ;;
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
  shift
done

# ─── packages ────────────────────────────────────────────────────────────────
install_packages() {
  [ "$(id -u)" -eq 0 ] || die "installing packages needs root"
  . /etc/os-release
  [ "${ID:-}" = debian ] && [ "${VERSION_ID:-}" = 12 ] \
    || log "WARNING: tested on Debian 12 only (found ${PRETTY_NAME:-unknown}); Dovecot must be 2.3"
  export DEBIAN_FRONTEND=noninteractive
  echo "postfix postfix/main_mailer_type select Internet Site" | debconf-set-selections
  echo "postfix postfix/mailname string ${MTA_HOSTNAME:-localhost}" | debconf-set-selections
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends \
    postfix postfix-pgsql \
    dovecot-core dovecot-imapd dovecot-lmtpd dovecot-pgsql dovecot-sieve dovecot-managesieved \
    amavisd-new spamassassin clamav-daemon clamav-freshclam libdbd-pg-perl \
    file unzip bzip2 \
    ssl-cert postgresql-client perl ca-certificates
}

if [ "$PACKAGES_ONLY" -eq 1 ]; then install_packages; log "packages installed"; exit 0; fi
if [ "$PACKAGES" -eq 1 ]; then install_packages; fi

# ─── settings ────────────────────────────────────────────────────────────────
[ -r "$ENV_FILE" ] || die "cannot read $ENV_FILE (copy deploy/mta.env.example there)"
set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a

for v in MTA_HOSTNAME MTA_MYNETWORKS MTA_INET_PROTOCOLS MTA_RELAYHOST MTA_MESSAGE_SIZE_LIMIT \
         POSTFIX_MAILLOG_FILE DOVECOT_LOG_PATH DOVECOT_MAIL_LOCATION_EXTRA TLS_CERT TLS_KEY \
         DB_HOST DB_PORT DB_NAME POSTFIX_DB_USER POSTFIX_DB_PASSWORD DOVECOT_DB_USER DOVECOT_DB_PASSWORD \
         VMAIL_USER VMAIL_UID VMAIL_GID VMAIL_BASE \
         MTA_CONTENT_FILTER MTA_REINJECT_BIND MTA_REINJECT_CLIENTS AMAVIS_DB_USER AMAVIS_DB_PASSWORD \
         AMAVIS_LOG_FILE AMAVIS_DO_SYSLOG AMAVIS_BYPASS_VIRUS_CHECKS AMAVIS_MAX_SERVERS \
         QUARANTINE_DIR BAYES_DIR CLAMD_SOCKET SPAMASSASSIN_SKIP_NETWORK \
         MAIL_MASTER_USER MAIL_MASTER_PASSWORD MAIL_MASTER_ALLOW_NETS; do
  [ "${!v+set}" = set ] || die "$v is not set in $ENV_FILE"
done
for v in POSTFIX_DB_PASSWORD DOVECOT_DB_PASSWORD AMAVIS_DB_PASSWORD MAIL_MASTER_PASSWORD; do
  case "${!v}" in CHANGE_ME|"") die "$v still has its placeholder value" ;; esac
done
case "$VMAIL_UID$VMAIL_GID" in *[!0-9]*|"") die "VMAIL_UID and VMAIL_GID must be numbers" ;; esac
case "$VMAIL_BASE" in /*) ;; *) die "VMAIL_BASE must be an absolute path" ;; esac
for v in QUARANTINE_DIR BAYES_DIR AMAVIS_LOG_FILE CLAMD_SOCKET; do
  case "${!v}" in /*) ;; *) die "$v must be an absolute path" ;; esac
  case "${!v}" in *[[:space:]\'\"\\]*) die "$v must not contain whitespace, quotes or backslashes" ;; esac
done
# The master login is written into a colon-separated file and checked in IMAP/SASL strings.
for v in MAIL_MASTER_USER MAIL_MASTER_PASSWORD; do
  case "${!v}" in *[[:space:]:*\'\"\\{}]*|"") die "$v must not be empty or contain whitespace, ':', '*', quotes, braces or backslashes" ;; esac
done
case "$MAIL_MASTER_ALLOW_NETS" in *[[:space:]]*|"") die "MAIL_MASTER_ALLOW_NETS must be comma separated, without spaces" ;; esac
case "$MTA_REINJECT_CLIENTS" in *[[:space:]]*|"") die "MTA_REINJECT_CLIENTS must be comma separated, without spaces" ;; esac
for v in AMAVIS_DO_SYSLOG AMAVIS_BYPASS_VIRUS_CHECKS AMAVIS_MAX_SERVERS SPAMASSASSIN_SKIP_NETWORK; do
  case "${!v}" in *[!0-9]*|"") die "$v must be a number" ;; esac
done
# These values are written into config files that treat whitespace and quotes
# specially; refuse the ones that would break the format.
for v in POSTFIX_DB_USER POSTFIX_DB_PASSWORD DOVECOT_DB_USER DOVECOT_DB_PASSWORD AMAVIS_DB_USER AMAVIS_DB_PASSWORD DB_NAME DB_HOST; do
  case "${!v}" in *[[:space:]\'\"\\]*|"") die "$v must not be empty or contain whitespace, quotes or backslashes" ;; esac
done

# Template list: "source|destination|mode|group"
TEMPLATES=(
  "postfix/main.cf.j2|/etc/postfix/main.cf|0644|root"
  "postfix/master.cf.j2|/etc/postfix/master.cf|0644|root"
  "postfix/pgsql/virtual-domains.cf.j2|/etc/postfix/pgsql/virtual-domains.cf|0640|postfix"
  "postfix/pgsql/virtual-mailboxes.cf.j2|/etc/postfix/pgsql/virtual-mailboxes.cf|0640|postfix"
  "postfix/pgsql/virtual-aliases.cf.j2|/etc/postfix/pgsql/virtual-aliases.cf|0640|postfix"
  "postfix/pgsql/virtual-groups.cf.j2|/etc/postfix/pgsql/virtual-groups.cf|0640|postfix"
  "postfix/pgsql/sender-login.cf.j2|/etc/postfix/pgsql/sender-login.cf|0640|postfix"
  "dovecot/dovecot.conf.j2|/etc/dovecot/dovecot.conf|0644|root"
  "dovecot/dovecot-sql.conf.ext.j2|/etc/dovecot/dovecot-sql.conf.ext|0640|dovecot"
  "dovecot/dovecot-dict-sql.conf.ext.j2|/etc/dovecot/dovecot-dict-sql.conf.ext|0640|dovecot"
  "dovecot/master-users.j2|/etc/dovecot/master-users|0640|dovecot"
  "dovecot/sieve/10-spam-to-junk.sieve.j2|/etc/dovecot/sieve/10-spam-to-junk.sieve|0644|root"
  "dovecot/sieve/learn-spam.sieve.j2|/etc/dovecot/sieve/learn-spam.sieve|0644|root"
  "dovecot/sieve/learn-ham.sieve.j2|/etc/dovecot/sieve/learn-ham.sieve|0644|root"
  "dovecot/rustmail-learn.sh.j2|/usr/lib/rustmail/sieve-pipe/rustmail-learn|0755|root"
  "amavis/60-rustmail.j2|/etc/amavis/conf.d/60-rustmail|0640|amavis"
  "spamassassin/local.cf.j2|/etc/spamassassin/local.cf|0644|root"
  "clamav/rustmail-eicar.ndb.j2|/var/lib/clamav/rustmail-eicar.ndb|0644|root"
)

# Replace {{ name }} with $NAME. An unset variable is an error, never an empty string.
render() {
  perl -e '
    my ($src) = @ARGV; local $/; open(my $fh, "<", $src) or die "$src: $!\n";
    my $t = <$fh>; my @missing;
    $t =~ s/\{\{\s*([a-z0-9_]+)\s*\}\}/
      my $k = uc($1);
      if (exists $ENV{$k}) { $ENV{$k} } else { push @missing, $k; "" }/ge;
    die "$src: undefined variable(s): @missing\n" if @missing;
    die "$src: unrendered template syntax left over\n" if $t =~ /\{\{|\{%/;
    print $t;' "$1"
}

if [ -n "$RENDER_TO" ]; then
  for t in "${TEMPLATES[@]}"; do
    IFS='|' read -r src dst _ _ <<<"$t"
    mkdir -p "$RENDER_TO$(dirname "$dst")"
    render "$ROOT/deploy/$src" >"$RENDER_TO$dst"
  done
  log "rendered ${#TEMPLATES[@]} files into $RENDER_TO"
  exit 0
fi

if [ "$CHECK" -eq 0 ] && [ "$(id -u)" -ne 0 ]; then die "run as root (or use --check / --render-to)"; fi

# ─── virtual mail user and storage ───────────────────────────────────────────
ensure_vmail() {
  if ! getent group "$VMAIL_GID" >/dev/null; then groupadd -g "$VMAIL_GID" "$VMAIL_USER"; fi
  if ! id -u "$VMAIL_USER" >/dev/null 2>&1; then
    # -o: the dev container may map vmail onto a host uid that already exists in the image.
    useradd -o -u "$VMAIL_UID" -g "$VMAIL_GID" -d "$VMAIL_BASE" -M -s /usr/sbin/nologin "$VMAIL_USER"
  fi
  if [ ! -d "$VMAIL_BASE" ]; then
    install -d -m 2770 -o "$VMAIL_UID" -g "$VMAIL_GID" "$VMAIL_BASE"
  elif [ "$CONTAINER" -eq 1 ]; then
    # Bind mount from the host: ownership belongs to the host user already.
    chmod 2770 "$VMAIL_BASE" 2>/dev/null || log "note: could not chmod $VMAIL_BASE (bind mount)"
  else
    chown "$VMAIL_UID:$VMAIL_GID" "$VMAIL_BASE"
    chmod 2770 "$VMAIL_BASE"
  fi
}

# ─── filtering pipeline: directories and group memberships ───────────────────
# Quarantine and Bayes data are shared between services through the vmail group:
#   quarantine : written by Amavis, read/released/deleted by RustMail (vmail group)
#   bayes      : written by Amavis (scanning) and by the mail user (Sieve learning)
ensure_pipeline() {
  local grp
  grp="$(getent group "$VMAIL_GID" | cut -d: -f1)"
  gpasswd -a amavis "$grp" >/dev/null
  # Debian's Amavis config reads the mail domain from here.
  [ -f /etc/mailname ] || printf '%s\n' "$MTA_HOSTNAME" >/etc/mailname
  # clamd reads the files Amavis hands it, so it joins Amavis's group.
  if id -u clamav >/dev/null 2>&1 && getent group amavis >/dev/null; then gpasswd -a clamav amavis >/dev/null; fi
  for d in "$QUARANTINE_DIR" "$BAYES_DIR"; do
    if [ ! -d "$d" ]; then install -d -m 2770 -o amavis -g "$VMAIL_GID" "$d"; fi
    chmod 2770 "$d" 2>/dev/null || log "note: could not chmod $d (bind mount)"
  done
  install -d -m 0755 "$(dirname "$AMAVIS_LOG_FILE")"
  if [ "$AMAVIS_DO_SYSLOG" -eq 0 ]; then
    touch "$AMAVIS_LOG_FILE"; chown amavis:"$grp" "$AMAVIS_LOG_FILE"; chmod 0640 "$AMAVIS_LOG_FILE"
  fi
}

# ─── database roles ──────────────────────────────────────────────────────────
# Credentials go to psql on stdin and through PG* variables, never in argv.
setup_db() {
  [ -n "${DB_ADMIN_URL:-}" ] || die "DB_ADMIN_URL is not set (needed to create the database roles; use --skip-db to skip)"
  eval "$(DB_ADMIN_URL="$DB_ADMIN_URL" perl -e '
    my $u = $ENV{DB_ADMIN_URL};
    $u =~ m{^postgres(?:ql)?://([^:@/]+)(?::([^@]*))?@([^:/]+)(?::(\d+))?/([^?]+)} or die "DB_ADMIN_URL: unsupported format\n";
    my %v = (PGUSER=>$1, PGPASSWORD=>$2, PGHOST=>$3, PGPORT=>$4, PGDATABASE=>$5);
    for my $k (sort keys %v) { next unless defined $v{$k}; my $x = $v{$k}; $x =~ s/%([0-9A-Fa-f]{2})/chr(hex($1))/ge; $x =~ s/\x27/\x27\\\x27\x27/g; print "export $k=\x27$x\x27\n"; }')"
  {
    perl -e 'for my $k (@ARGV) { (my $v = $ENV{$k}) =~ s/\x27/\x27\x27/g; (my $n = lc $k) =~ s/^(postfix|dovecot|amavis)_db_/$1_/; print "\\set $n \x27$v\x27\n"; }' \
      POSTFIX_DB_USER POSTFIX_DB_PASSWORD DOVECOT_DB_USER DOVECOT_DB_PASSWORD AMAVIS_DB_USER AMAVIS_DB_PASSWORD
    cat "$ROOT/deploy/postgres/mta-roles.sql"
  } | psql -X -q -v ON_ERROR_STOP=1 >/dev/null
  unset PGPASSWORD
  log "database roles up to date"
}

# ─── install files ───────────────────────────────────────────────────────────
STAGE="$(mktemp -d)"; BACKUP=""; CHANGED=0; DRIFT=0
trap 'rm -rf "$STAGE"' EXIT
declare -a INSTALLED=()

install_files() {
  BACKUP="/var/backups/rustmail-mta/$(date +%Y%m%d-%H%M%S)"
  for t in "${TEMPLATES[@]}"; do
    IFS='|' read -r src dst mode grp <<<"$t"
    render "$ROOT/deploy/$src" >"$STAGE/new" || die "rendering $src failed"
    if [ -f "$dst" ] && cmp -s "$STAGE/new" "$dst" \
       && [ "$(stat -c %a "$dst")" = "${mode#0}" ] && [ "$(stat -c %G "$dst")" = "$grp" ]; then continue; fi
    DRIFT=1
    [ "$CHECK" -eq 1 ] && { log "differs: $dst"; continue; }
    if [ -f "$dst" ]; then mkdir -p "$BACKUP$(dirname "$dst")"; cp -p "$dst" "$BACKUP$dst"; fi
    install -d "$(dirname "$dst")"
    install -m "$mode" -o root -g "$grp" "$STAGE/new" "$dst"
    INSTALLED+=("$dst"); CHANGED=1
    log "updated $dst"
  done
}

restore_backup() {
  [ -d "$BACKUP" ] || return 0
  ( cd "$BACKUP" && find . -type f ) | while read -r f; do cp -p "$BACKUP/$f" "/${f#./}"; done
  log "restored previous configuration from $BACKUP"
}

# Compile Sieve scripts so Dovecot never has to (it cannot write next to them).
compile_sieve() {
  for f in /etc/dovecot/sieve/*.sieve; do sievec "$f" || return 1; done
}

validate() {
  compile_sieve || return 1
  postfix check 2>&1 | sed 's/^/[postfix check] /' >&2; [ "${PIPESTATUS[0]}" -eq 0 ] || return 1
  doveconf -n >/dev/null || return 1
}

apply_reload() {
  [ "$RELOAD" -eq 1 ] && [ "$CHANGED" -eq 1 ] || return 0
  if [ -d /run/systemd/system ]; then
    local units="postfix dovecot amavis"
    # ClamAV is skipped when the virus scan is switched off (small machines).
    if [ "$AMAVIS_BYPASS_VIRUS_CHECKS" -eq 0 ]; then units="clamav-daemon clamav-freshclam $units"; fi
    # shellcheck disable=SC2086
    systemctl enable --now $units >/dev/null 2>&1 || true
    # Amavis and ClamAV read their settings and signatures at start-up: restart them.
    systemctl restart amavis
    if [ "$AMAVIS_BYPASS_VIRUS_CHECKS" -eq 0 ]; then systemctl restart clamav-daemon; fi
    systemctl reload-or-restart postfix dovecot
    log "services reloaded"
  else
    log "no systemd: restart postfix and dovecot yourself"
  fi
}

if [ "$CHECK" -eq 0 ]; then ensure_vmail; ensure_pipeline; fi
if [ "$DB" -eq 1 ]; then setup_db; fi
install_files
if [ "$CHECK" -eq 1 ]; then
  [ "$DRIFT" -eq 0 ] && { log "installed files match the templates"; exit 0; }
  exit 1
fi
if [ "$CHANGED" -eq 1 ]; then
  if ! validate; then restore_backup; die "configuration did not validate; previous files restored"; fi
fi
apply_reload
[ "$CHANGED" -eq 1 ] && log "done (configuration changed; backup in $BACKUP)" || log "done (no changes)"
