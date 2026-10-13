"""Read task lists exported by tasks 0.3 (CSV: id,title,done)."""

import csv
from pathlib import Path

from tasks.model import Task


def read_csv(path: Path) -> list[Task]:
    with path.open(newline="") as handle:
        return [
            Task(id=int(row["id"]), title=row["title"], done=row["done"] == "1")
            for row in csv.DictReader(handle)
        ]
