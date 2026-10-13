from tasks import store
from tasks.model import Task


def test_round_trip(tmp_path):
    path = tmp_path / "tasks.json"
    store.save([Task(id=1, title="Ship it")], path)
    assert store.load(path) == [Task(id=1, title="Ship it")]


def test_next_id_follows_the_highest():
    assert store.next_id([Task(id=4, title="a"), Task(id=2, title="b")]) == 5
