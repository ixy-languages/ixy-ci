.PHONY: start
start: config.toml runner-bin
	env RUST_LOG=info,ixy_ci=trace cargo run --release

.PHONY: runner-bin
runner-bin:
	cd runner && \
		cargo build --target x86_64-unknown-linux-musl --release && \
		cp target/x86_64-unknown-linux-musl/release/runner ../runner-bin

