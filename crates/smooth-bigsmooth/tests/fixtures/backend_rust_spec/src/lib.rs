use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub priority: String,
    pub status: String,
    pub tags: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateTask {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTask {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub status: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    pub priority: Option<String>,
}

type Db = Arc<Mutex<HashMap<String, Task>>>;

pub fn app() -> Router {
    let db: Db = Arc::new(Mutex::new(HashMap::new()));
    Router::new()
        .route("/health", get(health))
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/:id", get(get_task).patch(update_task).delete(delete_task))
        .with_state(db)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn create_task(State(db): State<Db>, Json(payload): Json<serde_json::Value>) -> impl IntoResponse {
    // title is required
    let title = match payload.get("title").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({"error": "title is required"}))).into_response();
        }
    };

    let description = payload.get("description").and_then(|v| v.as_str()).map(|s| s.to_string());

    let priority = payload.get("priority").and_then(|v| v.as_str()).unwrap_or("medium").to_string();

    let tags: Vec<String> = payload
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let task = Task {
        id: Uuid::new_v4().to_string(),
        title,
        description,
        priority,
        status: "open".to_string(),
        tags,
        created_at: Utc::now().to_rfc3339(),
    };

    let mut store = db.lock().unwrap();
    store.insert(task.id.clone(), task.clone());

    (StatusCode::CREATED, Json(task)).into_response()
}

async fn get_task(State(db): State<Db>, Path(id): Path<String>) -> impl IntoResponse {
    let store = db.lock().unwrap();
    match store.get(&id) {
        Some(task) => (StatusCode::OK, Json(task.clone())).into_response(),
        None => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response(),
    }
}

async fn list_tasks(State(db): State<Db>, Query(query): Query<ListQuery>) -> impl IntoResponse {
    let store = db.lock().unwrap();
    let tasks: Vec<Task> = store
        .values()
        .filter(|t| {
            if let Some(ref s) = query.status {
                if &t.status != s {
                    return false;
                }
            }
            if let Some(ref p) = query.priority {
                if &t.priority != p {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();
    Json(tasks)
}

async fn update_task(State(db): State<Db>, Path(id): Path<String>, Json(payload): Json<UpdateTask>) -> impl IntoResponse {
    let mut store = db.lock().unwrap();
    match store.get_mut(&id) {
        Some(task) => {
            if let Some(title) = payload.title {
                task.title = title;
            }
            if let Some(description) = payload.description {
                task.description = Some(description);
            }
            if let Some(priority) = payload.priority {
                task.priority = priority;
            }
            if let Some(status) = payload.status {
                task.status = status;
            }
            if let Some(tags) = payload.tags {
                task.tags = tags;
            }
            (StatusCode::OK, Json(task.clone())).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response(),
    }
}

async fn delete_task(State(db): State<Db>, Path(id): Path<String>) -> impl IntoResponse {
    let mut store = db.lock().unwrap();
    match store.remove(&id) {
        Some(_) => StatusCode::NO_CONTENT.into_response(),
        None => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "not found"}))).into_response(),
    }
}
