package taskapi

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"strings"
	"sync"
	"time"
)

const version = "0.1.0"

type Task struct {
	ID          string   `json:"id"`
	Title       string   `json:"title"`
	Description *string  `json:"description,omitempty"`
	Priority    string   `json:"priority"`
	Status      string   `json:"status"`
	Tags        []string `json:"tags"`
	CreatedAt   string   `json:"created_at"`
}

type taskStore struct {
	mu    sync.Mutex
	tasks map[string]*Task
}

func newTaskStore() *taskStore {
	return &taskStore{tasks: make(map[string]*Task)}
}

func newUUID() string {
	b := make([]byte, 16)
	_, _ = rand.Read(b)
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	return hex.EncodeToString(b[:4]) + "-" +
		hex.EncodeToString(b[4:6]) + "-" +
		hex.EncodeToString(b[6:8]) + "-" +
		hex.EncodeToString(b[8:10]) + "-" +
		hex.EncodeToString(b[10:])
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

// NewServer returns an http.Handler implementing the task API.
func NewServer() http.Handler {
	s := newTaskStore()
	mux := http.NewServeMux()

	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet {
			w.WriteHeader(http.StatusMethodNotAllowed)
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "ok", "version": version})
	})

	mux.HandleFunc("/tasks", func(w http.ResponseWriter, r *http.Request) {
		switch r.Method {
		case http.MethodGet:
			statusFilter := r.URL.Query().Get("status")
			priorityFilter := r.URL.Query().Get("priority")
			s.mu.Lock()
			result := make([]*Task, 0)
			for _, t := range s.tasks {
				if statusFilter != "" && t.Status != statusFilter {
					continue
				}
				if priorityFilter != "" && t.Priority != priorityFilter {
					continue
				}
				result = append(result, t)
			}
			s.mu.Unlock()
			writeJSON(w, http.StatusOK, result)

		case http.MethodPost:
			var body map[string]any
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				writeJSON(w, http.StatusBadRequest, map[string]string{"error": "invalid json"})
				return
			}
			titleVal, ok := body["title"]
			if !ok {
				writeJSON(w, http.StatusUnprocessableEntity, map[string]string{"error": "title is required"})
				return
			}
			title, ok := titleVal.(string)
			if !ok || title == "" {
				writeJSON(w, http.StatusUnprocessableEntity, map[string]string{"error": "title is required"})
				return
			}

			priority := "medium"
			if p, ok := body["priority"].(string); ok && p != "" {
				priority = p
			}

			var desc *string
			if d, ok := body["description"].(string); ok {
				desc = &d
			}

			tags := []string{}
			if rawTags, ok := body["tags"].([]any); ok {
				for _, rt := range rawTags {
					if sv, ok := rt.(string); ok {
						tags = append(tags, sv)
					}
				}
			}

			task := &Task{
				ID:          newUUID(),
				Title:       title,
				Description: desc,
				Priority:    priority,
				Status:      "open",
				Tags:        tags,
				CreatedAt:   time.Now().UTC().Format(time.RFC3339),
			}
			s.mu.Lock()
			s.tasks[task.ID] = task
			s.mu.Unlock()
			writeJSON(w, http.StatusCreated, task)

		default:
			w.WriteHeader(http.StatusMethodNotAllowed)
		}
	})

	mux.HandleFunc("/tasks/", func(w http.ResponseWriter, r *http.Request) {
		id := strings.TrimPrefix(r.URL.Path, "/tasks/")
		if id == "" {
			w.WriteHeader(http.StatusNotFound)
			return
		}

		switch r.Method {
		case http.MethodGet:
			s.mu.Lock()
			task, ok := s.tasks[id]
			s.mu.Unlock()
			if !ok {
				writeJSON(w, http.StatusNotFound, map[string]string{"error": "not found"})
				return
			}
			writeJSON(w, http.StatusOK, task)

		case http.MethodPatch:
			var body map[string]any
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				writeJSON(w, http.StatusBadRequest, map[string]string{"error": "invalid json"})
				return
			}
			s.mu.Lock()
			task, ok := s.tasks[id]
			if !ok {
				s.mu.Unlock()
				writeJSON(w, http.StatusNotFound, map[string]string{"error": "not found"})
				return
			}
			if v, ok := body["title"].(string); ok {
				task.Title = v
			}
			if v, ok := body["description"].(string); ok {
				task.Description = &v
			}
			if v, ok := body["priority"].(string); ok {
				task.Priority = v
			}
			if v, ok := body["status"].(string); ok {
				task.Status = v
			}
			if rawTags, ok := body["tags"].([]any); ok {
				tags := []string{}
				for _, rt := range rawTags {
					if sv, ok := rt.(string); ok {
						tags = append(tags, sv)
					}
				}
				task.Tags = tags
			}
			s.mu.Unlock()
			writeJSON(w, http.StatusOK, task)

		case http.MethodDelete:
			s.mu.Lock()
			_, ok := s.tasks[id]
			if !ok {
				s.mu.Unlock()
				writeJSON(w, http.StatusNotFound, map[string]string{"error": "not found"})
				return
			}
			delete(s.tasks, id)
			s.mu.Unlock()
			w.WriteHeader(http.StatusNoContent)

		default:
			w.WriteHeader(http.StatusMethodNotAllowed)
		}
	})

	return mux
}
