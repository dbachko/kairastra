.PHONY: install fmt fmt-check test clippy ci audit docker-build

install:
	./install.sh

fmt:
	cd rust && cargo fmt

fmt-check:
	cd rust && cargo fmt --check

test:
	cd rust && cargo test

clippy:
	cd rust && cargo clippy --all-targets --all-features -- -D warnings

ci: fmt-check test clippy

audit:
	bash scripts/public-readiness-audit.sh

docker-build:
	docker build -f rust/Dockerfile -t kairastra:local .
