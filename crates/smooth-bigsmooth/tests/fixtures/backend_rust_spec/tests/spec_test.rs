use reqwest::Client;
use serde_json::{json, Value};

async fn start_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}");
    tokio::spawn(async move {
        let app = task_api::app();
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    url
}

#[tokio::test]
async fn test_health() {
    let url = start_server().await;
    let resp = Client::new().get(format!("{url}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    // Must include a version field
    assert!(body["version"].is_string());
}

#[tokio::test]
async fn test_create_task_with_all_fields() {
    let url = start_server().await;
    let resp = Client::new().post(format!("{url}/tasks"))
        .json(&json!({
            "title": "Build feature X",
            "description": "Implement the new feature",
            "priority": "high",
            "tags": ["backend", "urgent"]
        }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "Build feature X");
    assert_eq!(body["description"], "Implement the new feature");
    assert_eq!(body["priority"], "high");
    assert_eq!(body["status"], "open");
    // Must have UUID id
    assert!(body["id"].as_str().unwrap().len() >= 8);
    // Must have created_at timestamp
    assert!(body["created_at"].is_string());
    // Must have tags array
    assert_eq!(body["tags"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_create_task_minimal() {
    let url = start_server().await;
    let resp = Client::new().post(format!("{url}/tasks"))
        .json(&json!({"title": "Simple task"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "Simple task");
    assert_eq!(body["priority"], "medium"); // default priority
    assert_eq!(body["status"], "open");
    assert_eq!(body["tags"].as_array().unwrap().len(), 0); // empty tags
}

#[tokio::test]
async fn test_missing_title_returns_error() {
    let url = start_server().await;
    let resp = Client::new().post(format!("{url}/tasks"))
        .header("content-type", "application/json")
        .body(r#"{"description": "no title"}"#)
        .send().await.unwrap();
    // Must reject — title is required
    assert!(resp.status() == 400 || resp.status() == 422);
}

#[tokio::test]
async fn test_get_task_by_id() {
    let url = start_server().await;
    let client = Client::new();
    let created: Value = client.post(format!("{url}/tasks"))
        .json(&json!({"title": "Find me"}))
        .send().await.unwrap().json().await.unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client.get(format!("{url}/tasks/{id}")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "Find me");
    assert_eq!(body["id"], id);
}

#[tokio::test]
async fn test_get_nonexistent_task_returns_404() {
    let url = start_server().await;
    let resp = Client::new().get(format!("{url}/tasks/nonexistent-id")).send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_list_all_tasks() {
    let url = start_server().await;
    let client = Client::new();
    client.post(format!("{url}/tasks")).json(&json!({"title": "Task A"})).send().await.unwrap();
    client.post(format!("{url}/tasks")).json(&json!({"title": "Task B"})).send().await.unwrap();
    client.post(format!("{url}/tasks")).json(&json!({"title": "Task C"})).send().await.unwrap();

    let resp = client.get(format!("{url}/tasks")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 3);
}

#[tokio::test]
async fn test_update_task_status() {
    let url = start_server().await;
    let client = Client::new();
    let created: Value = client.post(format!("{url}/tasks"))
        .json(&json!({"title": "Update me"}))
        .send().await.unwrap().json().await.unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client.patch(format!("{url}/tasks/{id}"))
        .json(&json!({"status": "in_progress"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "in_progress");
}

#[tokio::test]
async fn test_update_task_priority_and_title() {
    let url = start_server().await;
    let client = Client::new();
    let created: Value = client.post(format!("{url}/tasks"))
        .json(&json!({"title": "Old title", "priority": "low"}))
        .send().await.unwrap().json().await.unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client.patch(format!("{url}/tasks/{id}"))
        .json(&json!({"title": "New title", "priority": "critical"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "New title");
    assert_eq!(body["priority"], "critical");
}

#[tokio::test]
async fn test_delete_task() {
    let url = start_server().await;
    let client = Client::new();
    let created: Value = client.post(format!("{url}/tasks"))
        .json(&json!({"title": "Delete me"}))
        .send().await.unwrap().json().await.unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client.delete(format!("{url}/tasks/{id}")).send().await.unwrap();
    assert_eq!(resp.status(), 204);

    // Verify it's gone
    let resp = client.get(format!("{url}/tasks/{id}")).send().await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_filter_tasks_by_status() {
    let url = start_server().await;
    let client = Client::new();

    let t1: Value = client.post(format!("{url}/tasks"))
        .json(&json!({"title": "Open task"}))
        .send().await.unwrap().json().await.unwrap();
    let t2: Value = client.post(format!("{url}/tasks"))
        .json(&json!({"title": "Done task"}))
        .send().await.unwrap().json().await.unwrap();

    // Close the second task
    client.patch(format!("{url}/tasks/{}", t2["id"].as_str().unwrap()))
        .json(&json!({"status": "closed"}))
        .send().await.unwrap();

    // Filter by status=open
    let resp = client.get(format!("{url}/tasks?status=open")).send().await.unwrap();
    let body: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["title"], "Open task");
}

#[tokio::test]
async fn test_filter_tasks_by_priority() {
    let url = start_server().await;
    let client = Client::new();
    client.post(format!("{url}/tasks")).json(&json!({"title": "High", "priority": "high"})).send().await.unwrap();
    client.post(format!("{url}/tasks")).json(&json!({"title": "Low", "priority": "low"})).send().await.unwrap();

    let resp = client.get(format!("{url}/tasks?priority=high")).send().await.unwrap();
    let body: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["title"], "High");
}
