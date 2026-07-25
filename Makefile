SUPPLYCHAIN_SRC ?= $(HOME)/src/noeljackson/supplychain
SUPPLYCHAIN_REF := dd34170a75a80d9b3484e39df0ac2a5ebf6682dc

.PHONY: check supplychain supplychain-doctor test-docker

check: supplychain test-docker
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

supplychain:
	@SUPPLYCHAIN_SRC="$(SUPPLYCHAIN_SRC)" SUPPLYCHAIN_REF="$(SUPPLYCHAIN_REF)" ./bin/supplychain-check "$(CURDIR)"

supplychain-doctor:
	@"$(SUPPLYCHAIN_SRC)/supplychain" doctor

test-docker:
	docker build --target test --tag tachikoma:test .
	@set -euo pipefail; \
		docker compose up --build --detach; \
		trap 'docker compose down --volumes --remove-orphans' EXIT; \
		until curl --fail --silent --show-error http://127.0.0.1:17447/health >/dev/null; do sleep 1; done; \
		curl --fail --silent --show-error http://127.0.0.1:17447/ | grep -F 'Local policy proposal queue.' >/dev/null
