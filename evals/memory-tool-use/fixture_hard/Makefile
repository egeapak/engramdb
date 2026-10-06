.PHONY: test test-fast

test-fast:
	python -m pytest -m "not postgres and not integration" -q

test:
	python -m pytest -q
