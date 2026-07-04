"""FastAPI task API implementation."""

from __future__ import annotations

import uuid
from datetime import datetime, timezone
from typing import List, Optional

from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel

VERSION = "0.1.0"

app = FastAPI()

# In-memory store
_tasks: dict[str, dict] = {}


class CreateTaskBody(BaseModel):
    title: str
    description: Optional[str] = None
    priority: Optional[str] = "medium"
    tags: Optional[List[str]] = []


class UpdateTaskBody(BaseModel):
    title: Optional[str] = None
    description: Optional[str] = None
    priority: Optional[str] = None
    status: Optional[str] = None
    tags: Optional[List[str]] = None


@app.get("/health")
def health():
    return {"status": "ok", "version": VERSION}


@app.post("/tasks", status_code=201)
def create_task(body: CreateTaskBody):
    if not body.title or not body.title.strip():
        raise HTTPException(status_code=422, detail="title is required")
    task = {
        "id": str(uuid.uuid4()),
        "title": body.title,
        "priority": body.priority or "medium",
        "status": "open",
        "tags": body.tags or [],
        "created_at": datetime.now(timezone.utc).isoformat(),
    }
    if body.description is not None:
        task["description"] = body.description
    _tasks[task["id"]] = task
    return task


@app.get("/tasks")
def list_tasks(status: Optional[str] = None, priority: Optional[str] = None):
    result = list(_tasks.values())
    if status is not None:
        result = [t for t in result if t["status"] == status]
    if priority is not None:
        result = [t for t in result if t["priority"] == priority]
    return result


@app.get("/tasks/{task_id}")
def get_task(task_id: str):
    task = _tasks.get(task_id)
    if task is None:
        raise HTTPException(status_code=404, detail="not found")
    return task


@app.patch("/tasks/{task_id}")
def update_task(task_id: str, body: UpdateTaskBody):
    task = _tasks.get(task_id)
    if task is None:
        raise HTTPException(status_code=404, detail="not found")
    if body.title is not None:
        task["title"] = body.title
    if body.description is not None:
        task["description"] = body.description
    if body.priority is not None:
        task["priority"] = body.priority
    if body.status is not None:
        task["status"] = body.status
    if body.tags is not None:
        task["tags"] = body.tags
    return task


@app.delete("/tasks/{task_id}", status_code=204)
def delete_task(task_id: str):
    if task_id not in _tasks:
        raise HTTPException(status_code=404, detail="not found")
    del _tasks[task_id]
    return Response(status_code=204)
