"""Where settlement files come from."""
import tomllib
from pathlib import Path

CONFIG = tomllib.loads((Path(__file__).resolve().parent.parent / "ingest.toml").read_text())


def list_new_files(day):
    """Object keys of the settlement files for `day` (a date)."""
    raise NotImplementedError
