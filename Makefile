.PHONY: ci fmt clippy test drift catalog release

export PATH := $(HOME)/.cargo/bin:$(PATH)

ci: fmt clippy test drift

fmt:
	cargo fmt --all --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

# The committed catalog must match specs/; CI fails on drift.
drift:
	cargo xtask gen-catalog >/dev/null 2>&1
	git diff --exit-code -- crates/catalog/src/generated.rs

catalog:
	cargo xtask gen-catalog

# The release artefact. The target prints a receipt - the absolute path and the byte
# count of the stripped binary - so "it built" is something you can check rather than
# take on trust.
release:
	cargo build --release
	strip target/release/nutsh
	@printf 'path  %s\n' "$$(cd target/release && pwd)/nutsh"
	@printf 'bytes %s\n' "$$(wc -c < target/release/nutsh)"
