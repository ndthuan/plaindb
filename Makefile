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
	cargo run --release --locked --example git_size
	cargo bench --locked -p plaindb-core --features testgen
	cargo run --release --locked -p plaindb-core --features testgen --example memory
	cargo run --release --locked -p plaindb-core --features testgen --example heap
	cargo run --release --locked -p plaindb-core --example bench_table -- target/criterion
