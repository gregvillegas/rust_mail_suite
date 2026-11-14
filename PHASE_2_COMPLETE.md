# Phase 2 — Authentication & Session Layer — COMPLETE

## Summary

Login, refresh-token rotation with theft detection, logout, per-request
authentication, role checks, account lockout, CSRF protection, per-IP rate
limiting and an audit trail. `rustmail-core::auth` holds the logic with no HTTP
dependency; `rustmail-web` adds the cookies, middleware and `/auth/*` routes.
No Phase 3 work (templates, UI pages) was started.

## Deliverables

### `rustmail-core/src/auth/`
| File | Purpose |
|------|---------|
| `service.rs` | `AuthService`: `login`, `refresh`, `authenticate`, `logout`, `logout_by_refresh_token`, `logout_all`, `current_user`, `purge_expired_sessions` |
| `tokens.rs` | HS256 access JWTs (`issue_/verify_access_token`, `peek_claims`), refresh tokens (256-bit random, stored as SHA-256) |
| `csrf.rs` | `CsrfProtector`: signed double-submit tokens bound to a session |
| `config.rs` | `AuthConfig` (lifetimes, lockout, reuse grace); `from_env()` rejects weak or placeholder secrets |
| `error.rs` | `AuthError` (deliberately coarse variants) |

Also: `UserRole::at_least` for role checks, migration `0008` (`sessions.replaced_by`),
and repository additions (`rotate`, `find_by_token_hash`, `find_active_by_id`,
`record_login_success`, `record_login_failure`).

### `rustmail-web` (now a lib + bin)
`auth/` (cookies, CSRF middleware, rate limiter, `AuthUser` extractor, handlers),
`config.rs`, `build_app()`, and a minimal `main.rs` that serves `/healthz` and
`/auth/*` so the API can be exercised by hand.

| Route | Method | CSRF | Rate limited |
|-------|--------|:----:|:------------:|
| `/auth/csrf` | GET | n/a | no |
| `/auth/login` | POST | yes | yes |
| `/auth/refresh` | POST | yes | yes |
| `/auth/logout` | POST | yes | no |
| `/auth/me` | GET | n/a | no |

## Security model

| Concern | What is done |
|---------|--------------|
| Passwords | Argon2id (Phase 1 hashing); verification runs on the blocking pool so it cannot stall the async runtime. |
| Account enumeration | Unknown and wrong-password logins return the identical `401` body and both pay a full Argon2 verification (dummy hash). "Locked" and "disabled" are revealed only after the *correct* password. |
| Lockout | 5 consecutive failures lock for 15 min (configurable). Attempts during a lock do not extend it. |
| Access token | HS256 JWT, 15 min. Only HS256 is accepted (`alg: none` and algorithm-swap tests included). `exp` is checked against an injected clock. |
| Refresh token | 256-bit random, only its SHA-256 is stored. Every use rotates it. |
| Theft detection | Replaying an already-rotated token after a 10 s grace window revokes **all** of the user's sessions. Inside the window (two tabs racing) it is only rejected, so users are not logged out by a benign race. Replaying a token revoked by an ordinary logout is *not* treated as theft. |
| Immediate logout | `authenticate` checks the session is still active, so a logged-out access token stops working at once instead of living out its 15 minutes. |
| Cookies | `rm_access` HttpOnly/Lax/`/`; `rm_refresh` HttpOnly/**Strict**/path `/auth` (only sent to auth endpoints); `rm_csrf` readable by scripts by design. `Secure` is configurable. Tokens are never in response bodies. |
| CSRF | Signed double-submit: cookie must equal the `X-CSRF-Token` header and carry a valid HMAC for the caller's session (anonymous context before login). Login additionally requires `application/json`. |
| Rate limiting | Token bucket per client IP on login and refresh, applied *before* CSRF/credential work. IPv6 is bucketed per /64. With `TRUST_PROXY_HEADERS` only the **rightmost** `X-Forwarded-For` entry is used, so clients cannot spoof it. |
| Audit | `auth.login`, `auth.refresh`, `auth.refresh_reuse`, `auth.logout`, `auth.logout_all` with outcome, reason, IP and user agent (length-bounded). No passwords or tokens are ever written (a test scans the log for them). |
| Secrets in config | `JWT_SECRET` must be at least 32 bytes and not the `change-me` placeholder, or the server refuses to start. `Debug` output of configs, tokens and requests is redacted. |

## Verification

| Check | Result |
|-------|--------|
| `cargo build --workspace` (`RUSTFLAGS=-D warnings`, `SQLX_OFFLINE=true`) | pass |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass, 0 warnings |
| `cargo test --workspace --all-targets` | **115 passed, 0 failed** |
| Offline build with an unreachable `DATABASE_URL` (regenerated `.sqlx/`, 51 entries) | pass |
| Real `rustmail-web` binary over TCP, driven by a Python script (28 assertions) | all passed |

Test breakdown: `rustmail-core` 34 unit + 17 auth-service + 15 repository + 8 schema +
6 seed; `rustmail-web` 17 unit + 17 HTTP integration (`axum-test`); `rustmail-imap` 1.

The auth-service tests run against real PostgreSQL and cover: login, enumeration
parity, lockout and recovery, disabled user / suspended domain, rotation, role
refresh, theft detection, the grace window, a concurrent-refresh race (exactly one
winner, never two live sessions), expiry, logout idempotency, and audit hygiene.
The HTTP tests cover every row of the route table above, cookie attributes, six
distinct CSRF failure modes, 4xx handling of malformed bodies, role gating
(401/403/200), rate limiting per IP and per route, and `X-Forwarded-For` spoofing.

The smoke test (the real binary on a real socket, so the peer-address path is used)
confirmed: placeholder secret refused, CSRF enforced, cookie flags on the wire,
rotation, logout, replay of a pre-rotation token rejected, `429` on the 5th login
POST, a spoofed `X-Forwarded-For` ignored, and a graceful shutdown on SIGINT (exit 0).

### Reproduce
```bash
cp .env.example .env            # then put a real value in JWT_SECRET (make secret)
make dev && make migrate && make seed SEED_DOMAIN=example.com
make lint && make test
make run-web                    # then use /auth/* (see README)
```

## Things to know

1. **New dependencies**: `jsonwebtoken` 11.1 (feature `rust_crypto`), `sha2`, `hmac`,
   `base64`, `axum` 0.8, `axum-extra` 0.12 (cookie), `tower_governor` 0.8 (default
   `tonic` feature disabled), `time`, and `axum-test` 21 (dev). `tokio` gained the
   `net`, `signal` and `time` features. `jsonwebtoken`'s `rust_crypto` backend compiles
   RSA/EC crates even though only HS256 is used; I did not run `cargo audit`, so check
   it for advisories against those crates. Switching to the `aws_lc_rs` feature avoids
   them at the cost of a C build.
2. **Roles in tokens can lag.** A role change reaches an existing access token only at
   its next refresh (up to 15 min). Phase 7 must call `logout_all` when it changes a
   role or disables an account.
3. **`DomainAdmin` is only a rank.** `at_least` compares roles; nothing yet limits a
   domain admin to *their* domains. That needs an assignment table and lands in Phase 7.
4. **Sliding sessions.** Each rotation extends the session by the full refresh lifetime
   with no absolute cap. Theft detection limits the damage; a hard cap would need a
   session-family start time column.
5. **Audit writes are best-effort**, not fail-closed: a failure is logged and the login
   still succeeds. Say if you need the opposite for compliance.
6. **Rate-limit state is in memory, per process.** Fine for one instance; running several
   would need a shared store (Redis is already in the stack) later.
7. **Timing equalisation is by construction, not measured.** Both paths run one Argon2
   verification; I did not benchmark response-time variance.
8. **Not included:** password change/reset, password-policy beyond a 12-character
   minimum, re-hashing when Argon2 parameters change, `__Host-` cookie prefixes (they
   need `Secure` + path `/`, which conflicts with local HTTP and the path-scoped refresh
   cookie), security headers (HSTS, CSP: Phase 10) and the login page (Phase 3).
9. **`.env.example` sets `COOKIE_SECURE=false`** for local HTTP development. The code
   default is `true`; production must not copy the example value.
10. **A small `main.rs` shipped now** (it boots the auth API) so the work is testable by
    hand; Phase 3 builds the real application on it.

---

**Phase 2 complete. Say "Start Phase 3" to proceed.**
