SUPPLYCHAIN_SRC ?= $(HOME)/src/noeljackson/supplychain
SUPPLYCHAIN_REF := dd34170a75a80d9b3484e39df0ac2a5ebf6682dc
INSTALL_PREFIX ?= $(HOME)/.local
SYSTEMD_USER_DIR ?= $(HOME)/.config/systemd/user
INSTALL_BIN_DIR := $(INSTALL_PREFIX)/bin

.DEFAULT_GOAL := check

.PHONY: build-release check install service-enable service-logs service-restart service-status supplychain supplychain-doctor systemd-validate test-docker update

build-release:
	cargo build --release --locked -p tachikoma -p approval-daemon

check: supplychain test-docker
	python3 bin/check-boundaries.py
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

supplychain:
	@SUPPLYCHAIN_SRC="$(SUPPLYCHAIN_SRC)" SUPPLYCHAIN_REF="$(SUPPLYCHAIN_REF)" ./bin/supplychain-check "$(CURDIR)"

supplychain-doctor:
	@"$(SUPPLYCHAIN_SRC)/supplychain" doctor

systemd-validate:
	systemd-analyze verify contrib/systemd/tachikoma.service contrib/systemd/tachikoma-assessment.service contrib/systemd/tachikoma-slack.service

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
		contract_project="tachikoma-contract-$$$$"; \
		docker compose --project-name "$$contract_project" up --build --detach; \
		trap 'docker compose --project-name "$$contract_project" down --volumes --remove-orphans' EXIT; \
		until curl --fail --silent http://127.0.0.1:17447/health >/dev/null 2>&1; do sleep 1; done; \
		curl --fail --silent --show-error http://127.0.0.1:17447/approval/ | grep -F 'Reviewer credential' >/dev/null; \
		docker compose --project-name "$$contract_project" exec --no-TTY tachikoma tachikoma --rpc-socket /tmp/tachikoma.sock approval --credential /var/lib/tachikoma/approval/admin.token health | grep -F 'queues' >/dev/null; \
		docker compose --project-name "$$contract_project" restart tachikoma; \
		until curl --fail --silent http://127.0.0.1:17447/health >/dev/null 2>&1; do sleep 1; done; \
		docker compose --project-name "$$contract_project" exec --no-TTY tachikoma tachikoma tui --help | grep -F 'terminal UI' >/dev/null

# Build and install independent workers explicitly; this does not start or enable them.
.PHONY: build-platform install-workers
build-platform:
	cargo build --release --locked -p approval-daemon -p tachikoma -p snitchgate -p kubernetesgate -p tachikoma-assessment-worker -p tachikoma-slack

install-workers:
	cargo build --release --locked -p kubernetesgate -p tachikoma-assessment-worker -p tachikoma-slack
	install -Dm755 target/release/kubernetesgate "$(INSTALL_BIN_DIR)/kubernetesgate"
	install -Dm755 target/release/tachikoma-assessment-worker "$(INSTALL_BIN_DIR)/tachikoma-assessment-worker"
	install -Dm755 target/release/tachikoma-slack "$(INSTALL_BIN_DIR)/tachikoma-slack"
	install -Dm644 contrib/systemd/tachikoma-assessment.service "$(SYSTEMD_USER_DIR)/tachikoma-assessment.service"
	install -Dm644 contrib/systemd/tachikoma-slack.service "$(SYSTEMD_USER_DIR)/tachikoma-slack.service"
	systemctl --user daemon-reload
