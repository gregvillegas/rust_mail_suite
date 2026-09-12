# RustMail Suite — developer task runner.
# Portable with GNU Make 3.81 (the macOS system make).

COMPOSE := docker compose -f docker-compose.dev.yml

# Load .env if present so DATABASE_URL etc. are available to targets.
ifneq (,$(wildcard ./.env))
include .env
export
endif

.DEFAULT_GOAL := help
.PHONY: help dev dev-down dev-logs migrate test lint fmt build check clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

dev: ## Start dev infrastructure (Postgres 16, Redis 7, Mailpit)
	$(COMPOSE) up -d
	@echo "Postgres  : localhost:$${POSTGRES_PORT:-5432}"
	@echo "Redis     : localhost:$${REDIS_PORT:-6379}"
	@echo "Mailpit UI: http://localhost:$${MAILPIT_UI_PORT:-8025}"

dev-down: ## Stop dev infrastructure
	$(COMPOSE) down

dev-logs: ## Tail dev infrastructure logs
	$(COMPOSE) logs -f

migrate: ## Run database migrations (requires sqlx-cli; added in Phase 1)
	@command -v sqlx >/dev/null 2>&1 || { \
		echo "sqlx-cli not found. Install: cargo install sqlx-cli --no-default-features --features postgres"; \
		exit 1; }
	sqlx migrate run

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
