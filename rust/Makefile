COMPOSE_FILE := compose.yml
ENV_FILE := .env
SERVICE := symphony-rust
PROVIDER ?= codex

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
	@case "$(PROVIDER)" in \
	  codex) $(DOCKER_COMPOSE) exec $(SERVICE) codex login --device-auth ;; \
	  claude) $(DOCKER_COMPOSE) exec $(SERVICE) claude auth login ;; \
	  *) echo "Unsupported PROVIDER=$(PROVIDER) (expected codex or claude)" >&2; exit 1 ;; \
	esac

docker-login-codex:
	$(MAKE) docker-login PROVIDER=codex

docker-login-claude:
	$(MAKE) docker-login PROVIDER=claude
