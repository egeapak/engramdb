"""Apply migrations/ to the local preview database (var/dev.sqlite3)."""
import sqlite3
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DB = ROOT / "var" / "dev.sqlite3"


def main():
    DB.parent.mkdir(exist_ok=True)
    conn = sqlite3.connect(DB)
    conn.execute("CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY)")
    done = {r[0] for r in conn.execute("SELECT name FROM schema_migrations")}
    applied = 0
    for path in sorted((ROOT / "migrations").glob("*.sql")):
        if path.name in done:
            continue
        conn.executescript(path.read_text())
        conn.execute("INSERT INTO schema_migrations VALUES (?)", (path.name,))
        applied += 1
    conn.commit()
    print(f"applied {applied} migrations to {DB.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
