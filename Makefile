.PHONY: start
start: config.toml runner-bin
	env RUST_LOG=info,ixy_ci=trace cargo run --release -- --config config.toml

runner-bin:
	cd runner && cargo build --release && cp target/release/runner ../runner-bin

