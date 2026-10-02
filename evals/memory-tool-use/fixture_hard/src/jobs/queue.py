"""Thin wrapper around the background job backend."""

_BACKEND = None


def configure(backend):
    global _BACKEND
    _BACKEND = backend


def enqueue(name, payload, **options):
    """Enqueue a background job by name."""
    if _BACKEND is None:
        raise RuntimeError("job backend not configured")
    return _BACKEND.enqueue(name, payload, **options)
