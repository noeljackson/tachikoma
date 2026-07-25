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
		curl --fail --silent --show-error http://127.0.0.1:17447/ | grep -F 'Local policy proposal queue.' >/dev/null; \
		docker compose exec --no-TTY tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock status | grep -F 'opensnitch:' >/dev/null; \
		docker compose exec --no-TTY tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock policy upsert kubectl-development-read --adapter kubernetes --action review_kubectl_observation --scope '{"context":"development"}' --mode automatic --risk-ceiling low; \
		docker compose exec --no-TTY tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock suggest-kubectl --context development -- get pods; \
		docker compose exec --no-TTY tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock queue | grep -F 'kubernetes' >/dev/null; \
		docker compose exec --no-TTY tachikoma tachikoma tui --help | grep -F 'terminal UI' >/dev/null
