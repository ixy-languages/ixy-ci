.PHONY: start
start: config.toml runner/target/release/runner
	env RUST_LOG=info,ixy_ci=trace cargo run --release

runner/target/release/runner:
	cd runner && cargo build --release
