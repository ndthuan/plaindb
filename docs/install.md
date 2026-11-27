# Install


Download the archive for your platform from the [releases page](https://github.com/ndthuan/plaindb/releases): `aarch64-apple-darwin` for Macs with Apple silicon, `x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl` for Linux. The Linux binaries are static and run on any distribution. Check the download against the checksum file of the same release, then unpack it:

```
shasum -a 256 -c SHA256SUMS --ignore-missing
tar xzf plaindb-<version>-<target>.tar.gz
```

Put the `plaindb` binary on your `PATH`. To build it from source instead, with a Rust toolchain:

```
cargo install --git https://github.com/ndthuan/plaindb plaindb
```

`git` is needed only for the features that use it: history, time travel and backup.
