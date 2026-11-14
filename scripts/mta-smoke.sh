#!/usr/bin/env bash
# End-to-end check of the dev MTA (Phase 4): provisioning -> SMTP -> Dovecot IMAP.
#
# Needs: `make mta-up` running, a migrated dev database, python3, psql, and a
# built CLI (cargo build -p rustmail-cli). Creates a throw-away domain and
# removes it again on exit.
#
#   scripts/mta-smoke.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
# shellcheck disable=SC1091
[ -f .env ] && { set -a; . ./.env; set +a; }

: "${DATABASE_URL:=postgres://rustmail:rustmail@localhost:5433/rustmail}"
export DATABASE_URL SQLX_OFFLINE=false
export VMAIL_BASE="$ROOT/var/vhosts"
export QUARANTINE_DIR="$ROOT/var/quarantine"
LOG_DIR="$ROOT/var/log"
CLI="$ROOT/target/debug/rustmail-cli"
MTA=rustmail-mta
SMTP_PORT=2525 SUBMIT_PORT=5587 IMAPS_PORT=1993 SIEVE_PORT=4190
MAILPIT_API=http://localhost:${MAILPIT_UI_PORT:-8025}/api/v1

RUN="$(date +%s)"
DOMAIN="smoke${RUN}.test"
PW="correct-horse-battery-1"
PASS=0 FAIL=0

ok()   { PASS=$((PASS + 1)); printf '  ok    %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); printf '  FAIL  %s\n' "$1"; }
check() { # check "description" command...
  local d="$1"; shift
  if "$@" >/dev/null 2>&1; then ok "$d"; else bad "$d"; fi
}
sql() { PGPASSWORD="${PGPASSWORD:-rustmail}" psql -h localhost -p "${POSTGRES_PORT:-5433}" -U rustmail -d rustmail -X -q -tA "$@"; }

cleanup() {
  for f in $(sql -c "SELECT file_name FROM quarantine_items WHERE recipient LIKE '%@$DOMAIN'" 2>/dev/null); do
    rm -f "${QUARANTINE_DIR:?}/$f"
  done
  sql -c "DELETE FROM quarantine_items WHERE recipient LIKE '%@$DOMAIN'" \
      -c "DELETE FROM pipeline_events WHERE recipient LIKE '%@$DOMAIN' OR sender LIKE '%@$DOMAIN'" \
      -c "DELETE FROM filter_policies WHERE scope LIKE '%$DOMAIN'" >/dev/null 2>&1
  sql -c "DELETE FROM group_members WHERE group_id IN (SELECT id FROM groups WHERE domain_id IN (SELECT id FROM domains WHERE name='$DOMAIN'))" \
      -c "DELETE FROM groups WHERE domain_id IN (SELECT id FROM domains WHERE name='$DOMAIN')" \
      -c "DELETE FROM aliases WHERE domain_id IN (SELECT id FROM domains WHERE name='$DOMAIN')" \
      -c "DELETE FROM users WHERE domain_id IN (SELECT id FROM domains WHERE name='$DOMAIN')" \
      -c "DELETE FROM domains WHERE name='$DOMAIN'" >/dev/null 2>&1
  rm -rf "${VMAIL_BASE:?}/$DOMAIN"
}
trap cleanup EXIT

# ─── python helpers (SMTP / IMAP / ManageSieve over the published ports) ─────
py() { python3 -c "$@"; }
IMAP_COUNT='
import imaplib, ssl, sys
host, port, user, pw, subject = sys.argv[1:6]
folder = sys.argv[6] if len(sys.argv) > 6 else "INBOX"
ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
m = imaplib.IMAP4_SSL(host, int(port), ssl_context=ctx)
m.login(user, pw); m.select(folder)
typ, data = m.search(None, "SUBJECT", subject)
print(len(data[0].split()))
'
IMAP_LOGIN='
import imaplib, ssl, sys
ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
m = imaplib.IMAP4_SSL(sys.argv[1], int(sys.argv[2]), ssl_context=ctx)
try:
    m.login(sys.argv[3], sys.argv[4]); print("accepted")
except imaplib.IMAP4.error:
    print("rejected")
'
IMAP_EXPUNGE='
import imaplib, ssl, sys
host, port, user, pw, subject = sys.argv[1:6]
ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
m = imaplib.IMAP4_SSL(host, int(port), ssl_context=ctx)
m.login(user, pw); m.select("INBOX")
typ, data = m.search(None, "SUBJECT", subject)
for n in data[0].split():
    m.store(n, "+FLAGS", "\\Deleted")
m.expunge()
'
IMAP_TO_JUNK='
import imaplib, ssl, sys
host, port, user, pw, run = sys.argv[1:6]
ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
m = imaplib.IMAP4_SSL(host, int(port), ssl_context=ctx)
m.login(user, pw)
# Unique per run: SpamAssassin ignores a message it has already learned.
msg = ("From: x@example.org\r\nTo: a@b.test\r\nSubject: learn-me\r\nMessage-ID: <learn-%s@smoke.test>\r\n\r\ncheap pills viagra casino winner prize lottery ticket%s\r\n" % (run, run)).encode()
m.append("INBOX", None, None, msg)
m.select("INBOX")
typ, data = m.search(None, "SUBJECT", "learn-me")
m.copy(data[0].split()[-1], "Junk")
'
SMTP_RCPT='
import smtplib, sys
host, port, sender, rcpt = sys.argv[1:5]
s = smtplib.SMTP(host, int(port)); s.ehlo()
code, _ = s.mail(sender)
code, msg = s.rcpt(rcpt)
print(code, msg.decode(errors="replace")[:90])
'
SUBMIT='
import smtplib, ssl, sys
host, port, login, pw, sender, rcpt, subject = sys.argv[1:8]
ctx = ssl.create_default_context(); ctx.check_hostname = False; ctx.verify_mode = ssl.CERT_NONE
s = smtplib.SMTP(host, int(port)); s.ehlo(); s.starttls(context=ctx); s.ehlo()
s.login(login, pw)
try:
    s.sendmail(sender, [rcpt], f"From: {sender}\r\nTo: {rcpt}\r\nSubject: {subject}\r\n\r\nbody\r\n")
    print("sent")
except smtplib.SMTPSenderRefused as e:
    print("refused", e.smtp_code)
except smtplib.SMTPRecipientsRefused as e:
    # Postfix applies the sender-login check while answering RCPT TO.
    print("refused", next(iter(e.recipients.values()))[0])
'
SIEVE_GREETING='
import socket, sys
s = socket.create_connection((sys.argv[1], int(sys.argv[2])), timeout=10)
print(s.recv(2048).decode(errors="replace"))
'

swaks_send() { # swaks_send from to subject [size_kb]
  docker exec "$MTA" bash -c '
    args=(--server 127.0.0.1 --port 25 --from "$1" --to "$2" --header "Subject: $3" --body "smoke test" --protocol ESMTP --hide-all)
    if [ -n "${4:-}" ]; then
      head -c "$(( $4 * 1024 ))" /dev/zero | tr "\0" x > /tmp/big.txt
      args+=(--attach-type text/plain --attach @/tmp/big.txt)
    fi
    swaks "${args[@]}"' _ "$@" 2>&1
}
wait_for() { # wait_for description seconds command...
  local d="$1" n="$2"; shift 2
  for _ in $(seq "$n"); do if "$@" >/dev/null 2>&1; then ok "$d"; return; fi; sleep 1; done
  bad "$d"
}
folder_has() { # folder_has user subject folder
  [ "$(py "$IMAP_COUNT" 127.0.0.1 "$IMAPS_PORT" "$1" "$PW" "$2" "$3")" -ge 1 ]
}
inbox_has() { [ "$(py "$IMAP_COUNT" 127.0.0.1 "$IMAPS_PORT" "$1" "$PW" "$2")" -ge 1 ]; }

echo "== preflight"
[ -x "$CLI" ] || { echo "build the CLI first: cargo build -p rustmail-cli"; exit 2; }
docker exec "$MTA" true 2>/dev/null || { echo "the mta container is not running: make mta-up"; exit 2; }
mkdir -p "$VMAIL_BASE"

echo "== provisioning ($DOMAIN)"
"$CLI" domain create "$DOMAIN" >/dev/null 2>&1 && ok "domain created" || bad "domain created"
for u in alice bob carol; do
  "$CLI" user create "$u@$DOMAIN" --password "$PW" --quota-mb 50 >/dev/null 2>&1 && ok "user $u created" || bad "user $u created"
done
"$CLI" user create "tiny@$DOMAIN" --password "$PW" --quota-mb 1 >/dev/null 2>&1 && ok "user tiny (1 MB) created" || bad "user tiny created"
for d in cur new tmp; do check "Maildir/$d exists on disk" test -d "$VMAIL_BASE/$DOMAIN/alice/Maildir/$d"; done
"$CLI" mailbox check >/dev/null 2>&1 && ok "mailbox check clean" || bad "mailbox check clean"

echo "== authentication (Dovecot against the RustMail database)"
[ "$(py "$IMAP_LOGIN" 127.0.0.1 "$IMAPS_PORT" "alice@$DOMAIN" "$PW")" = accepted ] && ok "correct password accepted" || bad "correct password accepted"
[ "$(py "$IMAP_LOGIN" 127.0.0.1 "$IMAPS_PORT" "alice@$DOMAIN" "wrong-password-123")" = rejected ] && ok "wrong password rejected" || bad "wrong password rejected"
[ "$(py "$IMAP_LOGIN" 127.0.0.1 "$IMAPS_PORT" "Alice@$(printf %s "$DOMAIN" | tr a-z A-Z)" "$PW")" = accepted ] && ok "login is case-insensitive" || bad "login is case-insensitive"

echo "== delivery (swaks -> Postfix -> LMTP -> Dovecot)"
swaks_send "sender@example.org" "alice@$DOMAIN" "hello-$RUN" >/dev/null && ok "swaks: message accepted" || bad "swaks: message accepted"
wait_for "message appears in alice's IMAP INBOX" 15 inbox_has "alice@$DOMAIN" "hello-$RUN"
check "message file is in Maildir/new or cur" bash -c "grep -rl 'hello-$RUN' '$VMAIL_BASE/$DOMAIN/alice/Maildir/' | grep -q ."

echo "== rejection at SMTP time"
out="$(py "$SMTP_RCPT" 127.0.0.1 "$SMTP_PORT" sender@example.org "nobody@$DOMAIN")"
case "$out" in 550*5.1.1*) ok "unknown recipient: $out" ;; *) bad "unknown recipient got: $out" ;; esac
out="$(py "$SMTP_RCPT" 127.0.0.1 "$SMTP_PORT" sender@example.org "someone@not-ours.example")"
case "$out" in 5*|45*) ok "open relay refused: $out" ;; *) bad "open relay NOT refused: $out" ;; esac

echo "== aliases and groups"
did="$(sql -c "SELECT id FROM domains WHERE name='$DOMAIN'")"
sql -c "INSERT INTO aliases (domain_id, address, destination) VALUES ('$did', 'info@$DOMAIN', 'bob@$DOMAIN')" >/dev/null
swaks_send "sender@example.org" "info@$DOMAIN" "alias-$RUN" >/dev/null && ok "alias: accepted" || bad "alias: accepted"
wait_for "alias delivered to its destination (bob)" 15 inbox_has "bob@$DOMAIN" "alias-$RUN"
sql -c "INSERT INTO groups (domain_id, name, address) VALUES ('$did', 'team', 'team@$DOMAIN')" \
    -c "INSERT INTO group_members (group_id, user_id) SELECT g.id, u.id FROM groups g, users u WHERE g.address='team@$DOMAIN' AND u.email IN ('alice@$DOMAIN','carol@$DOMAIN')" >/dev/null
swaks_send "sender@example.org" "team@$DOMAIN" "group-$RUN" >/dev/null && ok "group: accepted" || bad "group: accepted"
wait_for "group mail reaches alice" 15 inbox_has "alice@$DOMAIN" "group-$RUN"
wait_for "group mail reaches carol" 15 inbox_has "carol@$DOMAIN" "group-$RUN"

echo "== account state"
sql -c "UPDATE users SET status='suspended' WHERE email='bob@$DOMAIN'" >/dev/null
out="$(py "$SMTP_RCPT" 127.0.0.1 "$SMTP_PORT" sender@example.org "bob@$DOMAIN")"
case "$out" in 550*) ok "suspended user: mail refused ($out)" ;; *) bad "suspended user got: $out" ;; esac
[ "$(py "$IMAP_LOGIN" 127.0.0.1 "$IMAPS_PORT" "bob@$DOMAIN" "$PW")" = rejected ] && ok "suspended user cannot log in" || bad "suspended user cannot log in"

echo "== submission (587: STARTTLS + SASL via Dovecot)"
out="$(py "$SUBMIT" 127.0.0.1 "$SUBMIT_PORT" "alice@$DOMAIN" "$PW" "alice@$DOMAIN" "outside@example.net" "submit-$RUN" 2>&1)"
[ "$out" = sent ] && ok "authenticated user can send" || bad "authenticated send: $out"
mailpit_has() { curl -fsS "$MAILPIT_API/search?query=subject:submit-$RUN" | grep -q '"messages_count":[1-9]'; }
wait_for "relayed message reached Mailpit" 20 mailpit_has
out="$(py "$SUBMIT" 127.0.0.1 "$SUBMIT_PORT" "alice@$DOMAIN" "$PW" "carol@$DOMAIN" "outside@example.net" "spoof-$RUN" 2>&1)"
case "$out" in refused*) ok "sender spoofing refused ($out)" ;; *) bad "sender spoofing allowed: $out" ;; esac
out="$(py "$SUBMIT" 127.0.0.1 "$SUBMIT_PORT" "alice@$DOMAIN" "wrong-password-123" "alice@$DOMAIN" "x@example.net" "bad-$RUN" 2>&1 | tail -1)"
case "$out" in sent*) bad "bad SASL password accepted" ;; *) ok "bad SASL password rejected" ;; esac

echo "== quota (Dovecot dict -> quota_usage)"
swaks_send "sender@example.org" "alice@$DOMAIN" "quota-ok-$RUN" >/dev/null
alice_msgs() { [ "$(sql -c "SELECT messages_count FROM quota_usage WHERE username='alice@$DOMAIN'")" -ge 2 ]; }
wait_for "quota_usage counts alice's messages" 15 alice_msgs
wait_for "quota-ok message was delivered (it passes through the filters first)" 30 inbox_has "alice@$DOMAIN" "quota-ok-$RUN"
sleep 1
before="$(sql -c "SELECT messages_count FROM quota_usage WHERE username='alice@$DOMAIN'")"
py "$IMAP_EXPUNGE" 127.0.0.1 "$IMAPS_PORT" "alice@$DOMAIN" "$PW" "quota-ok-$RUN"
sleep 2
after="$(sql -c "SELECT messages_count FROM quota_usage WHERE username='alice@$DOMAIN'")"
[ "$after" -lt "$before" ] && ok "deleting a message lowers the count ($before -> $after)" || bad "count after delete: $before -> $after"
swaks_send "sender@example.org" "tiny@$DOMAIN" "toobig-$RUN" 2048 >/dev/null
quota_refused() { docker logs "$MTA" 2>&1 | grep -qiE "tiny@$DOMAIN.*(Quota exceeded|552)"; }
wait_for "the over-quota message reached Dovecot and was refused" 90 quota_refused
[ "$(py "$IMAP_COUNT" 127.0.0.1 "$IMAPS_PORT" "tiny@$DOMAIN" "$PW" "toobig-$RUN")" = 0 ] && ok "over-quota message not delivered" || bad "over-quota message was delivered"
check "over-quota refusal logged" quota_refused

echo "== filtering: ClamAV + SpamAssassin, policy from PostgreSQL"
EICAR='X5O!P%@AP[4\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*'
GTUBE='XJS*C4JDBQADN1.NSBN3*2IDNEN*GTUBE-STANDARD-ANTI-UBE-TEST-EMAIL*C.34X'
"$CLI" user create "root@$DOMAIN" --password "$PW" --role super-admin >/dev/null 2>&1 && ok "super admin created" || bad "super admin created"
# carol's policy lets very high scores through (tagged, not held); everyone else keeps the default.
"$CLI" policy set "carol@$DOMAIN" --quarantine-level 5000 >/dev/null 2>&1 && ok "per-recipient policy saved" || bad "per-recipient policy saved"
swaks_body() { # swaks_body to subject body
  docker exec "$MTA" swaks --server 127.0.0.1 --port 25 --from "sender@example.org" --to "$1" \
    --header "Subject: $2" --body "$3" --protocol ESMTP --hide-all >/dev/null 2>&1
}
swaks_body "alice@$DOMAIN" "eicar-$RUN" "$EICAR" && ok "EICAR accepted by Postfix" || bad "EICAR accepted by Postfix"
swaks_body "alice@$DOMAIN" "gtube-$RUN" "$GTUBE" && ok "GTUBE spam accepted by Postfix" || bad "GTUBE spam accepted by Postfix"
swaks_body "alice@$DOMAIN" "legit-$RUN" "Lunch on Thursday?" && ok "legitimate mail accepted" || bad "legitimate mail accepted"
swaks_body "carol@$DOMAIN" "gtube-lenient-$RUN" "$GTUBE" && ok "spam for the lenient recipient accepted" || bad "spam for the lenient recipient accepted"
wait_for "legitimate mail is delivered" 30 inbox_has "alice@$DOMAIN" "legit-$RUN"
wait_for "lenient recipient's spam is delivered, tagged and filed in Junk" 30 folder_has "carol@$DOMAIN" "gtube-lenient-$RUN" Junk
[ "$(py "$IMAP_COUNT" 127.0.0.1 "$IMAPS_PORT" "carol@$DOMAIN" "$PW" "gtube-lenient-$RUN")" = 0 ] && ok "tagged spam is not in the Inbox" || bad "tagged spam is not in the Inbox"
[ "$(py "$IMAP_COUNT" 127.0.0.1 "$IMAPS_PORT" "alice@$DOMAIN" "$PW" "eicar-$RUN")" = 0 ] && ok "EICAR was not delivered" || bad "EICAR was not delivered"
[ "$(py "$IMAP_COUNT" 127.0.0.1 "$IMAPS_PORT" "alice@$DOMAIN" "$PW" "gtube-$RUN")" = 0 ] && ok "default-policy spam was not delivered" || bad "default-policy spam was not delivered"
[ "$(py "$IMAP_COUNT" 127.0.0.1 "$IMAPS_PORT" "alice@$DOMAIN" "$PW" "gtube-$RUN" Junk)" = 0 ] && ok "default-policy spam is not in Junk either" || bad "default-policy spam is not in Junk either"

echo "== pipeline log -> events and quarantine index"
ingest() { "$CLI" pipeline ingest --log "$LOG_DIR/amavis.log,$LOG_DIR/postfix.log" --log-timezone utc 2>&1; }
held() { sql -c "SELECT count(*) FROM quarantine_items WHERE recipient='alice@$DOMAIN' AND status='held' AND kind='$1'"; }
for _ in $(seq 20); do ingest >/dev/null; [ "$(held virus)" = 1 ] && [ "$(held spam)" = 1 ] && break; sleep 1; done
[ "$(held virus)" = 1 ] && ok "EICAR is in the quarantine index as a virus" || bad "EICAR is in the quarantine index as a virus"
[ "$(held spam)" = 1 ] && ok "spam is in the quarantine index" || bad "spam is in the quarantine index"
[ "$(sql -c "SELECT reason FROM quarantine_items WHERE recipient='alice@$DOMAIN' AND kind='virus'")" != "" ] && ok "the virus name was recorded" || bad "the virus name was recorded"
[ "$(sql -c "SELECT count(*) FROM quarantine_items WHERE recipient='carol@$DOMAIN'" 2>/dev/null)" = 0 ] && ok "tagged (not held) spam is not quarantined" || bad "tagged (not held) spam is not quarantined"
[ "$(sql -c "SELECT count(*) FROM pipeline_events WHERE recipient='alice@$DOMAIN' AND verdict='CLEAN'")" -ge 1 ] && ok "clean delivery recorded as an event" || bad "clean delivery recorded as an event"
[ "$(sql -c "SELECT count(*) FROM pipeline_events WHERE recipient='nobody@$DOMAIN' AND kind='reject'")" -ge 1 ] && ok "the SMTP rejection was recorded" || bad "the SMTP rejection was recorded"
[ "$(sql -c "SELECT count(*) FROM pipeline_events WHERE recipient='alice@$DOMAIN' AND status='sent'")" -ge 1 ] && ok "Postfix delivery status recorded" || bad "Postfix delivery status recorded"
ingest | grep -q "Stored 0 events" && ok "a second pass stores nothing (position remembered)" || bad "a second pass stores nothing (position remembered)"
for f in $(sql -c "SELECT file_name FROM quarantine_items WHERE recipient='alice@$DOMAIN'"); do
  check "quarantine file $f exists" test -f "$QUARANTINE_DIR/$f"
done

echo "== quarantine actions"
spam_id="$(sql -c "SELECT id FROM quarantine_items WHERE recipient='alice@$DOMAIN' AND kind='spam'")"
virus_id="$(sql -c "SELECT id FROM quarantine_items WHERE recipient='alice@$DOMAIN' AND kind='virus'")"
"$CLI" quarantine release "$spam_id" --by "root@$DOMAIN" >/dev/null 2>&1 && ok "release accepted by Postfix" || bad "release accepted by Postfix"
# A released spam still carries X-Spam-Flag, so Sieve files it in Junk like any other spam.
wait_for "released message reaches the recipient (in Junk)" 30 folder_has "alice@$DOMAIN" "gtube-$RUN" Junk
[ "$(sql -c "SELECT status FROM quarantine_items WHERE id='$spam_id'")" = released ] && ok "released item is marked released" || bad "released item is marked released"
virus_file="$(sql -c "SELECT file_name FROM quarantine_items WHERE id='$virus_id'")"
"$CLI" quarantine delete "$virus_id" --by "root@$DOMAIN" >/dev/null 2>&1 && ok "delete succeeded" || bad "delete succeeded"
[ ! -e "$QUARANTINE_DIR/$virus_file" ] && ok "deleted message's file is gone" || bad "deleted message's file is gone"
[ "$(sql -c "SELECT count(*) FROM audit_log WHERE action IN ('quarantine.release','quarantine.delete') AND actor_email='root@$DOMAIN'")" = 2 ] && ok "both actions are in the audit log" || bad "both actions are in the audit log"
"$CLI" quarantine release "$spam_id" --by "root@$DOMAIN" >/dev/null 2>&1 && bad "a second release is refused" || ok "a second release is refused"
"$CLI" quarantine release "$spam_id" --by "alice@$DOMAIN" >/dev/null 2>&1 && bad "only a super admin may act" || ok "only a super admin may act"

echo "== learning (move to Junk trains the Bayesian filter)"
spam_count() { docker exec "$MTA" sa-learn --dbpath /var/lib/rustmail/bayes --dump magic 2>/dev/null | awk '/non-token data: nspam/ {print $3}'; }
before="$(spam_count)"
py "$IMAP_TO_JUNK" 127.0.0.1 "$IMAPS_PORT" "alice@$DOMAIN" "$PW" "$RUN"
sleep 3
docker exec "$MTA" runuser -u amavis -- sa-learn --dbpath /var/lib/rustmail/bayes --sync >/dev/null 2>&1
after="$(spam_count)"
[ "${after:-0}" -gt "${before:-0}" ] && ok "moving a message to Junk taught SpamAssassin ($before -> $after)" || bad "Bayes spam count did not rise ($before -> $after)"

echo "== ManageSieve"
out="$(py "$SIEVE_GREETING" 127.0.0.1 "$SIEVE_PORT" 2>&1)"
case "$out" in *IMPLEMENTATION*STARTTLS*|*STARTTLS*IMPLEMENTATION*) ok "ManageSieve greets and offers STARTTLS" ;; *) bad "ManageSieve greeting: $out" ;; esac

echo "== idempotency"
out="$(docker exec "$MTA" /opt/rustmail/scripts/setup-mta.sh --env-file /opt/rustmail/deploy/dev/mta.env --container --skip-packages 2>&1)"
case "$out" in *"done (no changes)"*) ok "second setup run changes nothing" ;; *) bad "second setup run: $out" ;; esac

echo
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ]
