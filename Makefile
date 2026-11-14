# RustMail Suite — developer task runner.
# Portable with GNU Make 3.81 (the macOS system make).

COMPOSE := docker compose -f docker-compose.dev.yml

# Load .env if present so DATABASE_URL etc. are available to targets.
ifneq (,$(wildcard ./.env))
include .env
export
endif

.DEFAULT_GOAL := help
.PHONY: help dev dev-down dev-logs migrate seed secret run-web browser-test mta-up mta-down mta-smoke mta-check pipeline-ingest pipeline-follow prepare prepare-check test lint fmt build check clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

dev: ## Start dev infrastructure (Postgres 16, Redis 7, Mailpit)
	$(COMPOSE) up -d
	@echo "Postgres  : localhost:$${POSTGRES_PORT:-5433}"
	@echo "Redis     : localhost:$${REDIS_PORT:-6379}"
	@echo "Mailpit UI: http://localhost:$${MAILPIT_UI_PORT:-8025}"

dev-down: ## Stop dev infrastructure
	$(COMPOSE) down

dev-logs: ## Tail dev infrastructure logs
	$(COMPOSE) logs -f

migrate: ## Apply database migrations (needs the dev DB running)
	@command -v sqlx >/dev/null 2>&1 || { \
		echo "sqlx-cli not found. Install: cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features rustls,postgres"; \
		exit 1; }
	sqlx migrate run

SEED_DOMAIN ?= example.com

seed: ## Create the first domain + super admin (SEED_DOMAIN=example.com)
	cargo run -q -p rustmail-cli -- seed --domain $(SEED_DOMAIN)

secret: ## Print a fresh random value for JWT_SECRET
	@openssl rand -hex 32

run-web: ## Run the web server (needs the dev DB migrated and a real JWT_SECRET in .env)
	cargo run -p rustmail-web

browser-test: ## Real-browser smoke test (needs Chrome, internet, and built binaries; optional)
	cargo build -p rustmail-web -p rustmail-cli
	cd scripts/browser-smoke && npm install --no-audit --no-fund && npm test

mta-up: ## Start dev Postfix + Dovecot (Debian 12 container; needs `make dev`)
	@mkdir -p var/vhosts
	$(COMPOSE) --profile mta up -d --build mta
	@echo "SMTP 127.0.0.1:2525  submission 127.0.0.1:5587  IMAPS 127.0.0.1:1993  ManageSieve 127.0.0.1:4190"

mta-down: ## Stop the dev Postfix + Dovecot container
	$(COMPOSE) --profile mta stop mta

mta-check: ## Render every Postfix/Dovecot template and fail on undefined variables
	scripts/test-templates.sh

mta-smoke: ## End-to-end mail check: delivery, IMAP, quota, virus/spam filtering, quarantine (needs mta-up; ~7 min)
	cargo build -q -p rustmail-cli
	scripts/mta-smoke.sh

pipeline-ingest: ## Read the dev MTA logs once into events and the quarantine index
	cargo run -q -p rustmail-cli -- pipeline ingest

pipeline-follow: ## Keep reading the dev MTA logs (Ctrl-C to stop)
	cargo run -q -p rustmail-cli -- pipeline follow

prepare: ## Regenerate the offline sqlx query cache in .sqlx/ (needs a migrated DB)
	cargo sqlx prepare --workspace -- --all-targets

prepare-check: ## Fail if .sqlx/ is out of date (used by CI)
	cargo sqlx prepare --workspace --check -- --all-targets

test: ## Run the full test suite
	cargo test --workspace --all-targets

lint: ## Run rustfmt check + clippy (deny warnings)
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

fmt: ## Format all code
	cargo fmt --all

build: ## Build the whole workspace
	cargo build --workspace

check: ## Fast type-check without producing binaries
	cargo check --workspace

clean: ## Remove build artifacts
	cargo clean
