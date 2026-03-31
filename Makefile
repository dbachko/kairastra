.PHONY: docker-bootstrap-local docker-bootstrap-local-logs

docker-bootstrap-local:
	$(MAKE) -C rust docker-bootstrap-local

docker-bootstrap-local-logs:
	$(MAKE) -C rust docker-bootstrap-local-logs
