import tomllib
from pathlib import Path

_FLAGS = tomllib.loads((Path(__file__).parent.parent / "flags.toml").read_text())["flags"]


def is_on(name):
    return bool(_FLAGS.get(name, False))


def all_flags():
    return dict(_FLAGS)
