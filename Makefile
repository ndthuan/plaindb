.DEFAULT_GOAL := quality
.PHONY: quality soak bench

quality:
	cargo fmt --all --check
	cargo clippy --all-targets --all-features --locked -- -D warnings
	cargo test --locked --all-features
	cargo deny check

soak:
	cargo test --locked --all-features -- --ignored

bench:
	@echo no benchmarks yet
