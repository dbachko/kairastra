COMPOSE_FILE := compose.yml
ENV_FILE := .env
SERVICE := kairastra
PROVIDER ?=

DOCKER_COMPOSE := docker compose -f $(COMPOSE_FILE) --env-file $(ENV_FILE)

.PHONY: docker-build docker-up docker-down docker-logs docker-login docker-login-codex docker-login-claude

docker-build:
	$(DOCKER_COMPOSE) build

docker-up:
	$(DOCKER_COMPOSE) up -d --build

docker-down:
	$(DOCKER_COMPOSE) down

docker-logs:
	$(DOCKER_COMPOSE) logs -f $(SERVICE)

docker-login:
	@provider_arg=""; \
	if [ -n "$(PROVIDER)" ]; then \
	  provider_arg="--provider $(PROVIDER)"; \
	fi; \
	$(DOCKER_COMPOSE) run --rm $(SERVICE) auth $$provider_arg menu

docker-login-codex:
	$(MAKE) docker-login PROVIDER=codex

docker-login-claude:
	$(MAKE) docker-login PROVIDER=claude
