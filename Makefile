.PHONY: setup check fmt clippy test build

## First-time setup: configure git hooks
setup:
	git config core.hooksPath .githooks
	@echo "✅ Git hooks configured"

## Run all checks (same as CI)
check: fmt clippy test

fmt:
	cargo fmt -- --check

clippy:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test --all

build:
	cargo build --release
