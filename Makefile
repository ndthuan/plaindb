.DEFAULT_GOAL := quality
.PHONY: quality soak bench powerloss

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

LAZYFS_COMMIT := 045a0b3a1126725e693934e29d3ba15e08cc39ec
LAZYFS_DIR := target/lazyfs
POWERLOSS_EXPECT ?= pass

$(LAZYFS_DIR)/lazyfs/build/lazyfs:
	rm -rf $(LAZYFS_DIR)
	git init -q $(LAZYFS_DIR)
	git -C $(LAZYFS_DIR) fetch -q --depth 1 https://github.com/dsrhaslab/lazyfs.git $(LAZYFS_COMMIT)
	git -C $(LAZYFS_DIR) checkout -q FETCH_HEAD
	cd $(LAZYFS_DIR)/libs/libpcache && ./build.sh
	cd $(LAZYFS_DIR)/lazyfs && ./build.sh

powerloss: $(LAZYFS_DIR)/lazyfs/build/lazyfs
	scripts/powerloss.sh $(LAZYFS_DIR) target/powerloss $(POWERLOSS_EXPECT)
