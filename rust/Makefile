COMPOSE_FILE := compose.yml
ENV_FILE := .env
SERVICE := kairastra
PROVIDER ?=
COMPOSE_PROJECT ?= $(notdir $(CURDIR))
ENV_FILE_DIR := $(abspath $(dir $(ENV_FILE)))
ENV_FILE_BASENAME := $(notdir $(ENV_FILE))
HOST_REPO_ROOT := $(abspath ..)
CONFIG_VOLUME := $(COMPOSE_PROJECT)_kairastra_config
SEED_VOLUME := $(COMPOSE_PROJECT)_kairastra_seed

DOCKER_COMPOSE := docker compose -p $(COMPOSE_PROJECT) -f $(COMPOSE_FILE) --env-file $(ENV_FILE)

.PHONY: docker-build docker-up docker-down docker-logs docker-login docker-login-codex docker-login-claude docker-login-gemini docker-clean-oneoff docker-doctor docker-setup docker-sync-seed docker-config-export docker-config-import

docker-clean-oneoff:
	@ids="$$(docker ps -aq --filter label=com.docker.compose.project=$(COMPOSE_PROJECT) --filter label=com.docker.compose.oneoff=True)"; \
	if [ -n "$$ids" ]; then \
	  docker rm -f $$ids; \
	fi

docker-build:
	$(DOCKER_COMPOSE) build

docker-setup: docker-build docker-clean-oneoff
	$(DOCKER_COMPOSE) run --rm --remove-orphans -v $(ENV_FILE_DIR):/config-state $(SERVICE) setup --mode docker --workflow /config/WORKFLOW.md --env-file /config-state/$(ENV_FILE_BASENAME)

docker-sync-seed: docker-build docker-clean-oneoff
	$(DOCKER_COMPOSE) run --rm --remove-orphans --entrypoint bash -v $(HOST_REPO_ROOT):/seed-src:ro -v $(SEED_VOLUME):/seed-dest $(SERVICE) -lc 'set -euo pipefail; mkdir -p /seed-dest; rsync -a --delete /seed-src/ /seed-dest/'

docker-config-export: docker-build docker-clean-oneoff
	@if [ -z "$(DEST)" ]; then \
	  echo "DEST is required, e.g. make docker-config-export DEST=../WORKFLOW.exported.md" >&2; \
	  exit 1; \
	fi
	@mkdir -p "$$(dirname "$(DEST)")"
	$(DOCKER_COMPOSE) run --rm --remove-orphans --entrypoint bash -v $$(cd "$$(dirname "$(DEST)")" && pwd -P):/config-export $(SERVICE) -lc 'set -euo pipefail; cp /config/WORKFLOW.md /config-export/$$(basename "$(DEST)")'

docker-config-import: docker-build docker-clean-oneoff
	@if [ -z "$(SRC)" ]; then \
	  echo "SRC is required, e.g. make docker-config-import SRC=../WORKFLOW.md" >&2; \
	  exit 1; \
	fi
	$(DOCKER_COMPOSE) run --rm --remove-orphans --entrypoint bash -v $$(cd "$$(dirname "$(SRC)")" && pwd -P):/config-import:ro $(SERVICE) -lc 'set -euo pipefail; mkdir -p /config; cp /config-import/$$(basename "$(SRC)") /config/WORKFLOW.md'

docker-doctor: docker-clean-oneoff
	$(DOCKER_COMPOSE) run --rm --remove-orphans -v $(ENV_FILE_DIR):/config-state $(SERVICE) doctor --mode docker --workflow /config/WORKFLOW.md --env-file /config-state/$(ENV_FILE_BASENAME)

docker-up: docker-build docker-sync-seed docker-doctor
	$(DOCKER_COMPOSE) up -d

docker-down: docker-clean-oneoff
	$(DOCKER_COMPOSE) down --remove-orphans

docker-logs:
	$(DOCKER_COMPOSE) logs -f $(SERVICE)

docker-login: docker-clean-oneoff
	@provider_arg=""; \
	if [ -n "$(PROVIDER)" ]; then \
	  provider_arg="--provider $(PROVIDER)"; \
	fi; \
	$(DOCKER_COMPOSE) run --rm --remove-orphans $(SERVICE) auth $$provider_arg menu

docker-login-codex:
	$(MAKE) docker-login PROVIDER=codex

docker-login-claude:
	$(MAKE) docker-login PROVIDER=claude

docker-login-gemini:
	$(MAKE) docker-login PROVIDER=gemini
