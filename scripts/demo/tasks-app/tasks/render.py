from tasks.model import Task


def line(task: Task) -> str:
    mark = "x" if task.done else " "
    return f"[{mark}] {task.id:>3}  {task.title}"


def table(tasks: list[Task]) -> str:
    if not tasks:
        return "No tasks yet."
    return "\n".join(line(task) for task in tasks)
