.PHONY: check demo build benchmark

check:
	cargo fmt --all --check
	cargo clippy -p snitchgate --all-targets -- -D warnings
	$(MAKE) -C ../.. test-docker

demo:
	cargo run -p snitchgate -- demo

build:
	cargo build --release -p snitchgate

# Requires a local Ollama server and a model already present on disk.
BENCHMARK_OUTPUT ?= benchmarks/results/local-$(shell date +%Y%m%d-%H%M%S).json
benchmark:
	cargo run -p snitchgate -- benchmark --thinking off --output $(BENCHMARK_OUTPUT)
