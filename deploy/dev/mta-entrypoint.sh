#!/usr/bin/env bash
# Render the configuration, then run ClamAV, Amavis, Dovecot and Postfix.
set -euo pipefail

ENV_FILE=/opt/rustmail/deploy/dev/mta.env
mkdir -p /var/log/rustmail
/opt/rustmail/scripts/setup-mta.sh --env-file "$ENV_FILE" --container --skip-packages

set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a

# Postfix copies resolver files into its spool; the chroot-free master.cf needs
# only the queue directories.
postfix set-permissions >/dev/null 2>&1 || true
mkdir -p /var/lib/dovecot/indexes && chown vmail:vmail /var/lib/dovecot/indexes
install -d -o amavis -g amavis /run/amavis /var/lib/amavis
install -d -o clamav -g clamav /run/clamav

# Collect every service log in one place on the container's output as well.
touch /var/log/rustmail/postfix.log
tail -n 0 -F /var/log/rustmail/postfix.log /var/log/rustmail/amavis.log &

if [ "$AMAVIS_BYPASS_VIRUS_CHECKS" = 0 ]; then
  if [ "${CLAMAV_UPDATE:-0}" = 1 ]; then
    chown clamav:clamav /var/lib/clamav 2>/dev/null || chmod 0777 /var/lib/clamav
    (freshclam --foreground --quiet || echo "freshclam failed; continuing with the signatures already present" >&2) &
  fi
  clamd --foreground &
  # Amavis would treat a missing socket as a failed scan and defer all mail.
  for _ in $(seq 60); do [ -S "$CLAMD_SOCKET" ] && break; sleep 1; done
  [ -S "$CLAMD_SOCKET" ] || { echo "clamd did not start" >&2; exit 1; }
fi

# Debian's own unit runs it as the amavis user, reading /etc/amavis/conf.d.
runuser -u amavis -- /usr/sbin/amavisd foreground &
for _ in $(seq 30); do (exec 3<>/dev/tcp/127.0.0.1/10024) 2>/dev/null && break; sleep 1; done

dovecot -F &
postfix start-fg &
# If any one exits, stop the container so the failure is visible.
wait -n
echo "a mail service exited; stopping" >&2
exit 1
