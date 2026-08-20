# Lyra — Makefile
# Common developer tasks. Run `make help` (default) to list targets.

.DEFAULT_GOAL := help

.PHONY: help secretscan pre-commit-install

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

secretscan: ## Run gitleaks secret scan on full repo history
	@./scripts/secretscan.sh

pre-commit-install: ## Install pre-commit hooks (gitleaks)
	pre-commit install
	@echo "✓ pre-commit hooks installed. gitleaks will run on every commit."
