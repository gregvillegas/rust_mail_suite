-- Least-privilege database roles for the mail transfer stack.
--
-- postfix : reads routing data only. Cannot see password hashes.
-- amavis  : reads the filter policies and nothing else.
-- dovecot : reads what it needs to authenticate and locate mailboxes, and
--           maintains quota counters (a recalculation deletes and re-inserts them).
--
-- Run as a role that may create roles (the table owner or a superuser), after
-- the migrations. Safe to re-run: roles are created once, passwords are reset
-- and grants are re-applied. Re-run it after a migration adds tables.
--
--   psql -v ON_ERROR_STOP=1 -d rustmail \
--        -v postfix_user=... -v postfix_password=... \
--        -v dovecot_user=... -v dovecot_password=... \
--        -v amavis_user=... -v amavis_password=... < mta-roles.sql
--
-- Feed it on stdin (as above) so the passwords never appear in process arguments.

\set ON_ERROR_STOP on

SELECT format('CREATE ROLE %I LOGIN', :'postfix_user')
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'postfix_user')
\gexec
SELECT format('CREATE ROLE %I LOGIN', :'dovecot_user')
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'dovecot_user')
\gexec

SELECT format('ALTER ROLE %I WITH LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE', :'postfix_user', :'postfix_password')
\gexec
SELECT format('ALTER ROLE %I WITH LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE', :'dovecot_user', :'dovecot_password')
\gexec
SELECT format('CREATE ROLE %I LOGIN', :'amavis_user')
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'amavis_user')
\gexec
SELECT format('ALTER ROLE %I WITH LOGIN PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE', :'amavis_user', :'amavis_password')
\gexec

-- Start from nothing so a re-run also removes grants that were dropped here.
SELECT format('REVOKE ALL ON ALL TABLES IN SCHEMA public FROM %I, %I, %I', :'postfix_user', :'dovecot_user', :'amavis_user')
\gexec
SELECT format('GRANT USAGE ON SCHEMA public TO %I, %I, %I', :'postfix_user', :'dovecot_user', :'amavis_user')
\gexec

-- Postfix: column-level grants, no password_hash.
SELECT format('GRANT SELECT (id, name, status) ON domains TO %I', :'postfix_user') \gexec
SELECT format('GRANT SELECT (id, email, status, domain_id) ON users TO %I', :'postfix_user') \gexec
SELECT format('GRANT SELECT (user_id, status) ON mailboxes TO %I', :'postfix_user') \gexec
SELECT format('GRANT SELECT (address, destination, enabled, domain_id) ON aliases TO %I', :'postfix_user') \gexec
SELECT format('GRANT SELECT (id, address, domain_id) ON groups TO %I', :'postfix_user') \gexec
SELECT format('GRANT SELECT (group_id, user_id) ON group_members TO %I', :'postfix_user') \gexec

-- Dovecot: authentication and mailbox lookup.
SELECT format('GRANT SELECT (id, name, status, default_quota_bytes) ON domains TO %I', :'dovecot_user') \gexec
SELECT format('GRANT SELECT (id, email, password_hash, status, domain_id) ON users TO %I', :'dovecot_user') \gexec
SELECT format('GRANT SELECT (id, user_id, status, maildir_path, quota_bytes) ON mailboxes TO %I', :'dovecot_user') \gexec
-- Quota counters. The BEFORE INSERT trigger (migration 0009) runs with the caller's
-- rights and looks the mailbox up, which is why mailboxes.id and users.id are granted.
SELECT format('GRANT SELECT, INSERT, UPDATE, DELETE ON quota_usage TO %I', :'dovecot_user') \gexec

-- Amavis: reads the filter policies, nothing else.
SELECT format('GRANT SELECT (scope, spam_tag_level, spam_tag2_level, spam_kill_level, bypass_spam_checks, bypass_virus_checks, spam_lover, virus_lover, id) ON filter_policies TO %I', :'amavis_user') \gexec
