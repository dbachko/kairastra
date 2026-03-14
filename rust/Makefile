COMPOSE_FILE := compose.yml
ENV_FILE := .env
SERVICE := symphony-rust

DOCKER_COMPOSE := docker compose -f $(COMPOSE_FILE) --env-file $(ENV_FILE)

.PHONY: docker-build docker-up docker-down docker-logs docker-login

docker-build:
	$(DOCKER_COMPOSE) build

docker-up:
	$(DOCKER_COMPOSE) up -d --build

docker-down:
	$(DOCKER_COMPOSE) down

docker-logs:
	$(DOCKER_COMPOSE) logs -f $(SERVICE)

docker-login:
	$(DOCKER_COMPOSE) exec $(SERVICE) codex login
