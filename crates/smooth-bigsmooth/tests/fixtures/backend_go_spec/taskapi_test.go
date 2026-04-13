// Contract tests for a small Go HTTP task API.
//
// The agent is expected to write `taskapi.go` exporting a
// `func NewServer() http.Handler` that implements every endpoint the tests
// exercise. The tests drive the handler via `httptest.NewServer` — no real
// network listener needed, fully deterministic.
//
// This is the Go mirror of task_api_spec (Rust) and hono_api_spec
// (TypeScript).

package taskapi

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// req builds a Request against the handler under test and returns
// (status, body as map).
func req(t *testing.T, handler http.Handler, method, path string, body any) (int, map[string]any) {
	t.Helper()
	var bodyReader io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			t.Fatalf("marshal body: %v", err)
		}
		bodyReader = bytes.NewReader(b)
	}
	r, err := http.NewRequest(method, path, bodyReader)
	if err != nil {
		t.Fatalf("build request: %v", err)
	}
	if body != nil {
		r.Header.Set("Content-Type", "application/json")
	}
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, r)
	resp := w.Result()
	defer resp.Body.Close()
	out := map[string]any{}
	raw, _ := io.ReadAll(resp.Body)
	if len(raw) > 0 {
		_ = json.Unmarshal(raw, &out)
	}
	return resp.StatusCode, out
}

func reqList(t *testing.T, handler http.Handler, method, path string) (int, []map[string]any) {
	t.Helper()
	r, _ := http.NewRequest(method, path, nil)
	w := httptest.NewRecorder()
	handler.ServeHTTP(w, r)
	resp := w.Result()
	defer resp.Body.Close()
	var out []map[string]any
	raw, _ := io.ReadAll(resp.Body)
	_ = json.Unmarshal(raw, &out)
	return resp.StatusCode, out
}

func TestHealth(t *testing.T) {
	h := NewServer()
	status, body := req(t, h, "GET", "/health", nil)
	if status != 200 {
		t.Fatalf("expected 200, got %d", status)
	}
	if body["status"] != "ok" {
		t.Errorf("expected status=ok, got %v", body["status"])
	}
	if _, ok := body["version"].(string); !ok {
		t.Errorf("expected version string, got %v", body["version"])
	}
}

func TestCreateTaskMinimal(t *testing.T) {
	h := NewServer()
	status, body := req(t, h, "POST", "/tasks", map[string]any{"title": "Buy milk"})
	if status != 201 {
		t.Fatalf("expected 201, got %d (body: %v)", status, body)
	}
	if body["title"] != "Buy milk" {
		t.Errorf("title mismatch: %v", body["title"])
	}
	if body["status"] != "open" {
		t.Errorf("expected status=open, got %v", body["status"])
	}
	if body["priority"] != "medium" {
		t.Errorf("expected priority=medium, got %v", body["priority"])
	}
	if _, ok := body["id"].(string); !ok {
		t.Errorf("expected id string")
	}
	if _, ok := body["created_at"].(string); !ok {
		t.Errorf("expected created_at string")
	}
	tags, ok := body["tags"].([]any)
	if !ok {
		t.Errorf("expected tags array, got %v", body["tags"])
	}
	if len(tags) != 0 {
		t.Errorf("expected empty tags, got %v", tags)
	}
}

func TestCreateTaskMissingTitle(t *testing.T) {
	h := NewServer()
	status, _ := req(t, h, "POST", "/tasks", map[string]any{"description": "no title"})
	if status != 400 && status != 422 {
		t.Errorf("expected 400 or 422, got %d", status)
	}
}

func TestListTasks(t *testing.T) {
	h := NewServer()
	req(t, h, "POST", "/tasks", map[string]any{"title": "one"})
	req(t, h, "POST", "/tasks", map[string]any{"title": "two", "priority": "high"})
	status, list := reqList(t, h, "GET", "/tasks")
	if status != 200 {
		t.Fatalf("expected 200, got %d", status)
	}
	if len(list) < 2 {
		t.Errorf("expected at least 2 tasks, got %d", len(list))
	}
}

func TestGetTaskByID(t *testing.T) {
	h := NewServer()
	_, created := req(t, h, "POST", "/tasks", map[string]any{"title": "findable"})
	id, _ := created["id"].(string)
	status, body := req(t, h, "GET", "/tasks/"+id, nil)
	if status != 200 {
		t.Fatalf("expected 200, got %d", status)
	}
	if body["title"] != "findable" {
		t.Errorf("title mismatch: %v", body["title"])
	}
}

func TestGetNonexistentTask(t *testing.T) {
	h := NewServer()
	status, _ := req(t, h, "GET", "/tasks/does-not-exist", nil)
	if status != 404 {
		t.Errorf("expected 404, got %d", status)
	}
}

func TestFilterByStatus(t *testing.T) {
	h := NewServer()
	req(t, h, "POST", "/tasks", map[string]any{"title": "filterable"})
	status, list := reqList(t, h, "GET", "/tasks?status=open")
	if status != 200 {
		t.Fatalf("expected 200, got %d", status)
	}
	for _, task := range list {
		if task["status"] != "open" {
			t.Errorf("expected only open tasks, got %v", task["status"])
		}
	}
}

func TestFilterByPriority(t *testing.T) {
	h := NewServer()
	req(t, h, "POST", "/tasks", map[string]any{"title": "lo", "priority": "low"})
	req(t, h, "POST", "/tasks", map[string]any{"title": "hi", "priority": "high"})
	status, list := reqList(t, h, "GET", "/tasks?priority=high")
	if status != 200 {
		t.Fatalf("expected 200, got %d", status)
	}
	for _, task := range list {
		if task["priority"] != "high" {
			t.Errorf("expected only high priority, got %v", task["priority"])
		}
	}
}

func TestUpdateTaskStatus(t *testing.T) {
	h := NewServer()
	_, created := req(t, h, "POST", "/tasks", map[string]any{"title": "updatable"})
	id, _ := created["id"].(string)
	status, body := req(t, h, "PATCH", "/tasks/"+id, map[string]any{"status": "in_progress"})
	if status != 200 {
		t.Fatalf("expected 200, got %d", status)
	}
	if body["status"] != "in_progress" {
		t.Errorf("expected status=in_progress, got %v", body["status"])
	}
}

func TestDeleteTask(t *testing.T) {
	h := NewServer()
	_, created := req(t, h, "POST", "/tasks", map[string]any{"title": "doomed"})
	id, _ := created["id"].(string)
	r, _ := http.NewRequest("DELETE", "/tasks/"+id, nil)
	w := httptest.NewRecorder()
	h.ServeHTTP(w, r)
	if w.Code != 204 {
		t.Fatalf("expected 204, got %d", w.Code)
	}
	// Deleted task should 404.
	status, _ := req(t, h, "GET", "/tasks/"+id, nil)
	if status != 404 {
		t.Errorf("expected 404 after delete, got %d", status)
	}
}

func TestCreateTaskAllFields(t *testing.T) {
	h := NewServer()
	status, body := req(t, h, "POST", "/tasks", map[string]any{
		"title":       "Ship feature",
		"description": "Ship the refactor",
		"priority":    "high",
		"tags":        []string{"backend", "urgent"},
	})
	if status != 201 {
		t.Fatalf("expected 201, got %d", status)
	}
	if body["description"] != "Ship the refactor" {
		t.Errorf("description mismatch")
	}
	if body["priority"] != "high" {
		t.Errorf("priority mismatch")
	}
	tags, _ := body["tags"].([]any)
	if len(tags) != 2 {
		t.Errorf("expected 2 tags, got %d", len(tags))
	}
}

// Sanity: ensure the handler compiles and responds to unknown routes
// consistently (either 404 or 405, not 500).
func TestUnknownRouteNotServerError(t *testing.T) {
	h := NewServer()
	r, _ := http.NewRequest("GET", "/nothing/here", nil)
	w := httptest.NewRecorder()
	h.ServeHTTP(w, r)
	if w.Code >= 500 {
		body, _ := io.ReadAll(w.Result().Body)
		t.Errorf("unknown route should not 5xx, got %d: %s", w.Code, strings.TrimSpace(string(body)))
	}
}
