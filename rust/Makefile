COMPOSE_FILE := compose.yml
ENV_FILE := .env
SERVICE := kairastra
PROVIDER ?=
COMPOSE_PROJECT ?= $(notdir $(CURDIR))

DOCKER_COMPOSE := docker compose -p $(COMPOSE_PROJECT) -f $(COMPOSE_FILE) --env-file $(ENV_FILE)

.PHONY: docker-build docker-up docker-down docker-logs docker-login docker-login-codex docker-login-claude docker-login-gemini docker-clean-oneoff

docker-clean-oneoff:
	@ids="$$(docker ps -aq --filter label=com.docker.compose.project=$(COMPOSE_PROJECT) --filter label=com.docker.compose.oneoff=True)"; \
	if [ -n "$$ids" ]; then \
	  docker rm -f $$ids; \
	fi

docker-build:
	$(DOCKER_COMPOSE) build

docker-up:
	$(DOCKER_COMPOSE) up -d --build

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
