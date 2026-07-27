SUPPLYCHAIN_SRC ?= $(HOME)/src/noeljackson/supplychain
SUPPLYCHAIN_REF := dd34170a75a80d9b3484e39df0ac2a5ebf6682dc
INSTALL_PREFIX ?= $(HOME)/.local
SYSTEMD_USER_DIR ?= $(HOME)/.config/systemd/user
INSTALL_BIN_DIR := $(INSTALL_PREFIX)/bin

.DEFAULT_GOAL := check

.PHONY: build-release check install service-enable service-logs service-restart service-status supplychain supplychain-doctor systemd-validate test-docker update

build-release:
	cargo build --release --locked

check: supplychain test-docker
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

supplychain:
	@SUPPLYCHAIN_SRC="$(SUPPLYCHAIN_SRC)" SUPPLYCHAIN_REF="$(SUPPLYCHAIN_REF)" ./bin/supplychain-check "$(CURDIR)"

supplychain-doctor:
	@"$(SUPPLYCHAIN_SRC)/supplychain" doctor

systemd-validate:
	systemd-analyze verify contrib/systemd/tachikoma.service

install: build-release systemd-validate
	install -Dm755 target/release/tachikomad "$(INSTALL_BIN_DIR)/tachikomad"
	install -Dm755 target/release/tachikoma "$(INSTALL_BIN_DIR)/tachikoma"
	install -Dm644 contrib/systemd/tachikoma.service "$(SYSTEMD_USER_DIR)/tachikoma.service"
	systemctl --user daemon-reload

service-enable: install
	systemctl --user enable --now tachikoma.service

service-restart:
	systemctl --user restart tachikoma.service

service-status:
	systemctl --user status tachikoma.service

service-logs:
	journalctl --user --unit=tachikoma.service --follow

update:
	@set -eu; \
		if test -n "$$(git status --porcelain)"; then \
			echo "refusing to update a dirty worktree; commit, stash, or discard local changes first" >&2; \
			exit 1; \
		fi; \
		git pull --ff-only
	$(MAKE) check
	$(MAKE) install
	systemctl --user try-restart tachikoma.service

test-docker:
	docker build --target test --tag tachikoma:test .
	@set -euo pipefail; \
		docker compose up --build --detach; \
		trap 'docker compose down --volumes --remove-orphans' EXIT; \
		until curl --fail --silent http://127.0.0.1:17447/health >/dev/null 2>&1; do sleep 1; done; \
		curl --fail --silent --show-error http://127.0.0.1:17447/ | grep -F 'Local policy proposal queue.' >/dev/null; \
		docker compose exec --no-TTY tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock status | grep -F 'opensnitch:' >/dev/null; \
		docker compose exec --no-TTY tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock policy upsert kubectl-development-read --adapter kubernetes --action review_kubectl_observation --scope '{"context":"development"}' --mode automatic --risk-ceiling low; \
		docker compose exec --no-TTY tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock suggest-kubectl --context development -- get pods; \
		docker compose restart tachikoma; \
		until curl --fail --silent http://127.0.0.1:17447/health >/dev/null 2>&1; do sleep 1; done; \
		docker compose exec --no-TTY tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock queue | grep -F 'kubernetes' >/dev/null; \
		docker compose exec --no-TTY tachikoma tachikoma tui --help | grep -F 'terminal UI' >/dev/null
