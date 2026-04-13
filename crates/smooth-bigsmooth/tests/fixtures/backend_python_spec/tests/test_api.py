"""Contract tests for a small FastAPI task API.

The agent is expected to write ``taskapi.py`` exporting a ``app`` attribute
that is a FastAPI instance with every endpoint the tests exercise. The
tests drive the app via httpx's TestClient — no real server, no port,
fully deterministic.

This is the Python mirror of:
- task_api_spec (Rust / axum)
- hono_api_spec (TypeScript / Hono)
- backend_go_spec (Go / stdlib)
"""

import pytest
from fastapi.testclient import TestClient

from taskapi import app


@pytest.fixture
def client():
    return TestClient(app)


class TestHealth:
    def test_returns_status_and_version(self, client):
        r = client.get("/health")
        assert r.status_code == 200
        body = r.json()
        assert body["status"] == "ok"
        assert isinstance(body["version"], str)


class TestCreateTask:
    def test_minimal_body(self, client):
        r = client.post("/tasks", json={"title": "Buy milk"})
        assert r.status_code == 201
        body = r.json()
        assert body["title"] == "Buy milk"
        assert body["status"] == "open"
        assert body["priority"] == "medium"
        assert isinstance(body["id"], str)
        assert isinstance(body["created_at"], str)
        assert body["tags"] == []

    def test_all_fields(self, client):
        r = client.post(
            "/tasks",
            json={
                "title": "Ship feature",
                "description": "Ship the refactor",
                "priority": "high",
                "tags": ["backend", "urgent"],
            },
        )
        assert r.status_code == 201
        body = r.json()
        assert body["title"] == "Ship feature"
        assert body["description"] == "Ship the refactor"
        assert body["priority"] == "high"
        assert body["tags"] == ["backend", "urgent"]

    def test_missing_title_returns_error(self, client):
        r = client.post("/tasks", json={"description": "no title"})
        assert r.status_code in (400, 422)


class TestListTasks:
    def test_returns_all(self, client):
        client.post("/tasks", json={"title": "one"})
        client.post("/tasks", json={"title": "two", "priority": "high"})
        r = client.get("/tasks")
        assert r.status_code == 200
        assert isinstance(r.json(), list)
        assert len(r.json()) >= 2

    def test_filter_by_status(self, client):
        client.post("/tasks", json={"title": "filterable"})
        r = client.get("/tasks?status=open")
        assert r.status_code == 200
        for task in r.json():
            assert task["status"] == "open"

    def test_filter_by_priority(self, client):
        client.post("/tasks", json={"title": "lo", "priority": "low"})
        client.post("/tasks", json={"title": "hi", "priority": "high"})
        r = client.get("/tasks?priority=high")
        assert r.status_code == 200
        for task in r.json():
            assert task["priority"] == "high"


class TestGetTask:
    def test_by_id(self, client):
        created = client.post("/tasks", json={"title": "findable"}).json()
        r = client.get(f"/tasks/{created['id']}")
        assert r.status_code == 200
        assert r.json()["title"] == "findable"

    def test_unknown_id_returns_404(self, client):
        r = client.get("/tasks/does-not-exist")
        assert r.status_code == 404


class TestUpdateTask:
    def test_status(self, client):
        created = client.post("/tasks", json={"title": "updatable"}).json()
        r = client.patch(f"/tasks/{created['id']}", json={"status": "in_progress"})
        assert r.status_code == 200
        assert r.json()["status"] == "in_progress"

    def test_title_and_priority_together(self, client):
        created = client.post("/tasks", json={"title": "old title"}).json()
        r = client.patch(
            f"/tasks/{created['id']}",
            json={"title": "new title", "priority": "high"},
        )
        assert r.status_code == 200
        body = r.json()
        assert body["title"] == "new title"
        assert body["priority"] == "high"


class TestDeleteTask:
    def test_returns_204_and_removes(self, client):
        created = client.post("/tasks", json={"title": "doomed"}).json()
        r = client.delete(f"/tasks/{created['id']}")
        assert r.status_code == 204
        check = client.get(f"/tasks/{created['id']}")
        assert check.status_code == 404
