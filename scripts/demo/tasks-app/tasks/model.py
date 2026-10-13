from dataclasses import dataclass


@dataclass
class Task:
    id: int
    title: str
    done: bool = False

    def complete(self) -> None:
        self.done = True
