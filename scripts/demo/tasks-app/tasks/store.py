import json
from pathlib import Path

from tasks.model import Task

DEFAULT_PATH = Path.home() / ".tasks.json"


def load(path: Path = DEFAULT_PATH) -> list[Task]:
    if not path.exists():
        return []
    rows = json.loads(path.read_text())
    return [Task(id=row["id"], title=row["title"], done=row["done"]) for row in rows]


def save(tasks: list[Task], path: Path = DEFAULT_PATH) -> None:
    rows = [{"id": t.id, "title": t.title, "done": t.done} for t in tasks]
    path.write_text(json.dumps(rows, indent=2))


def next_id(tasks: list[Task]) -> int:
    return max((t.id for t in tasks), default=0) + 1
