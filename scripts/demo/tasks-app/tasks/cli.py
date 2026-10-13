import argparse
from pathlib import Path

from tasks import render, store
from tasks.legacy_import import read_csv
from tasks.model import Task


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="tasks")
    commands = parser.add_subparsers(dest="command", required=True)
    add = commands.add_parser("add", help="add a task")
    add.add_argument("title")
    commands.add_parser("list", help="list tasks")
    done = commands.add_parser("done", help="complete a task")
    done.add_argument("id", type=int)
    legacy = commands.add_parser("import", help="import a tasks 0.3 CSV export")
    legacy.add_argument("path", type=Path)
    args = parser.parse_args(argv)

    tasks = store.load()
    if args.command == "add":
        task = Task(id=store.next_id(tasks), title=args.title)
        tasks.append(task)
        print(render.line(task))
    elif args.command == "list":
        print(render.table(tasks))
    elif args.command == "done":
        for task in tasks:
            if task.id == args.id:
                task.complete()
                print(render.line(task))
    elif args.command == "import":
        tasks.extend(read_csv(args.path))
    store.save(tasks)
