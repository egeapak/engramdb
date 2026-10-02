import re
import unittest
from pathlib import Path

FLAGS_TOML = Path(__file__).resolve().parent.parent / "flags.toml"


class FlagOwnershipTest(unittest.TestCase):
    def test_every_flag_has_an_owner(self):
        lines = FLAGS_TOML.read_text().splitlines()
        for i, line in enumerate(lines):
            m = re.match(r"^([a-z0-9-]+)\s*=", line)
            if not m:
                continue
            above = lines[i - 1].strip() if i else ""
            self.assertTrue(
                above.startswith("# owner:"),
                f"flag {m.group(1)!r} has no `# owner: <team>` comment on the line above it. "
                "Every flag needs an owning team (ADR-014): the quarterly sweep deletes unowned flags.",
            )


if __name__ == "__main__":
    unittest.main()
