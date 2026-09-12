# RustMail Suite — developer task runner.
# Usage: make <target>. Run `make help` to list targets.

COMPOSE      ?= docker compose
COMPOSE_FILE ?= docker-compose.dev.yml
CARGO        ?= cargo

# Load DATABASE_URL from .env if present (used by migrate).
ifneq (,$(wildcard .env))
include .env
export
endif

.DEFAULT_GOAL := help

.PHONY: help dev down logs migrate test lint fmt fmt-check clippy build clean

help: ## Show this help
	@echo "RustMail Suite — available targets:"
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

dev: ## Start dev infrastructure (Postgres 16, Redis 7, Mailpit)
	$(COMPOSE) -f $(COMPOSE_FILE) up -d
	@echo "Dev stack up. Mailpit UI: http://localhost:$${MAILPIT_UI_PORT:-8025}"

down: ## Stop dev infrastructure
	$(COMPOSE) -f $(COMPOSE_FILE) down

logs: ## Tail dev infrastructure logs
	$(COMPOSE) -f $(COMPOSE_FILE) logs -f

migrate: ## Run database migrations (requires sqlx-cli + DATABASE_URL)
	@command -v sqlx >/dev/null 2>&1 || { \
		echo "sqlx-cli not found. Install with: cargo install sqlx-cli --no-default-features --features native-tls,postgres"; \
		exit 1; }
	sqlx migrate run

build: ## Build the entire workspace
	$(CARGO) build --workspace

test: ## Run the workspace test suite
	$(CARGO) test --workspace

lint: fmt-check clippy ## Run formatting check + clippy

fmt: ## Format the workspace
	$(CARGO) fmt --all

fmt-check: ## Check formatting without modifying files
	$(CARGO) fmt --all -- --check

clippy: ## Run clippy with warnings denied
	$(CARGO) clippy --workspace --all-targets -- -D warnings

clean: ## Remove build artifacts
	$(CARGO) clean
