# Lyra — Makefile
# Common developer tasks. Run `make help` (default) to list targets.

.DEFAULT_GOAL := help

.PHONY: help secretscan pre-commit-install fmt fmt-check lint check frontend-fmt frontend-lint backend-fmt backend-lint

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

secretscan: ## Run gitleaks secret scan on full repo history
	@./scripts/secretscan.sh

pre-commit-install: ## Install pre-commit hooks (gitleaks)
	pre-commit install
	@echo "✓ pre-commit hooks installed. gitleaks will run on every commit."

frontend-fmt: ## Format frontend (Prettier)
	cd frontend && npm run format

frontend-lint: ## Lint + typecheck frontend (oxlint, tsc)
	cd frontend && npm run check

backend-fmt: ## Format backend (rustfmt)
	cd backend && cargo fmt

backend-lint: ## Lint backend (clippy, warnings as errors)
	cd backend && cargo clippy --all-targets --all-features -- -D warnings

fmt: frontend-fmt backend-fmt ## Format frontend and backend

fmt-check: ## Check formatting without writing
	cd frontend && npm run format:check
	cd backend && cargo fmt -- --check

lint: frontend-lint backend-lint ## Lint frontend and backend

check: fmt-check lint secretscan ## Format check + lint + secret scan
