// smooth-dolt: embedded Dolt database for Smooth Pearls.
//
// A tiny CLI wrapping Dolt's embedded driver so `th pearls` can use
// full Dolt (clone, push, pull, merge, log, diff) without requiring
// the external `dolt` CLI. Built from source in the smooth repo.
//
// Usage:
//
//	smooth-dolt init <data-dir>
//	smooth-dolt sql  <data-dir> -q "SELECT ..."
//	smooth-dolt commit <data-dir> -m "message"
//	smooth-dolt push <data-dir>
//	smooth-dolt pull <data-dir>
//	smooth-dolt clone <remote-url> <data-dir>
//	smooth-dolt log <data-dir> [--oneline] [-n N]
//	smooth-dolt remote <data-dir> add <name> <url>
//	smooth-dolt remote <data-dir> list
//	smooth-dolt gc <data-dir>
//	smooth-dolt status <data-dir>
//	smooth-dolt serve <data-dir> --socket <path>
//	smooth-dolt version

package main

import (
	"bufio"
	"database/sql"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"sync"
	"syscall"

	_ "github.com/dolthub/driver"
)

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(1)
	}

	cmd := os.Args[1]

	switch cmd {
	case "version":
		fmt.Println("smooth-dolt 0.1.0 (embedded dolt)")
		os.Exit(0)
	case "init":
		if len(os.Args) < 3 {
			fatal("usage: smooth-dolt init <data-dir>")
		}
		cmdInit(os.Args[2])
	case "sql":
		if len(os.Args) < 4 {
			fatal("usage: smooth-dolt sql <data-dir> -q \"SQL\"")
		}
		dataDir := os.Args[2]
		query := findFlag(os.Args[3:], "-q")
		if query == "" {
			fatal("missing -q flag")
		}
		cmdSQL(dataDir, query)
	case "commit":
		if len(os.Args) < 4 {
			fatal("usage: smooth-dolt commit <data-dir> -m \"message\"")
		}
		dataDir := os.Args[2]
		msg := findFlag(os.Args[3:], "-m")
		if msg == "" {
			fatal("missing -m flag")
		}
		cmdCommit(dataDir, msg)
	case "log":
		if len(os.Args) < 3 {
			fatal("usage: smooth-dolt log <data-dir> [-n N] [--oneline]")
		}
		dataDir := os.Args[2]
		n := 20
		if nStr := findFlag(os.Args[3:], "-n"); nStr != "" {
			if v, err := strconv.Atoi(nStr); err == nil {
				n = v
			}
		}
		cmdLog(dataDir, n)
	case "push":
		if len(os.Args) < 3 {
			fatal("usage: smooth-dolt push <data-dir>")
		}
		cmdDoltCmd(os.Args[2], "push")
	case "pull":
		if len(os.Args) < 3 {
			fatal("usage: smooth-dolt pull <data-dir>")
		}
		cmdDoltCmd(os.Args[2], "pull")
	case "clone":
		if len(os.Args) < 4 {
			fatal("usage: smooth-dolt clone <remote-url> <data-dir>")
		}
		cmdClone(os.Args[2], os.Args[3])
	case "remote":
		if len(os.Args) < 4 {
			fatal("usage: smooth-dolt remote <data-dir> add|list ...")
		}
		cmdRemote(os.Args[2], os.Args[3:])
	case "gc":
		if len(os.Args) < 3 {
			fatal("usage: smooth-dolt gc <data-dir>")
		}
		cmdDoltCmd(os.Args[2], "gc")
	case "status":
		if len(os.Args) < 3 {
			fatal("usage: smooth-dolt status <data-dir>")
		}
		cmdDoltCmd(os.Args[2], "status")
	case "serve":
		if len(os.Args) < 3 {
			fatal("usage: smooth-dolt serve <data-dir> --socket <path>")
		}
		dataDir := os.Args[2]
		socket := findFlag(os.Args[3:], "--socket")
		if socket == "" {
			fatal("missing --socket flag")
		}
		cmdServe(dataDir, socket)
	default:
		fmt.Fprintf(os.Stderr, "unknown command: %s\n", cmd)
		usage()
		os.Exit(1)
	}
}

func usage() {
	fmt.Fprintln(os.Stderr, "smooth-dolt: embedded Dolt for Smooth Pearls")
	fmt.Fprintln(os.Stderr, "")
	fmt.Fprintln(os.Stderr, "Commands:")
	fmt.Fprintln(os.Stderr, "  init <dir>              Initialize a new Dolt database")
	fmt.Fprintln(os.Stderr, "  sql <dir> -q \"SQL\"      Execute SQL and print results as JSON")
	fmt.Fprintln(os.Stderr, "  commit <dir> -m \"msg\"   Stage all and commit")
	fmt.Fprintln(os.Stderr, "  log <dir> [-n N]        Show commit history")
	fmt.Fprintln(os.Stderr, "  push <dir>              Push to remote")
	fmt.Fprintln(os.Stderr, "  pull <dir>              Pull from remote")
	fmt.Fprintln(os.Stderr, "  clone <url> <dir>       Clone a remote database")
	fmt.Fprintln(os.Stderr, "  remote <dir> add|list   Manage remotes")
	fmt.Fprintln(os.Stderr, "  gc <dir>                Garbage collect")
	fmt.Fprintln(os.Stderr, "  status <dir>            Show working set status")
	fmt.Fprintln(os.Stderr, "  serve <dir> --socket    Long-running server: open DB once, accept JSON-line")
	fmt.Fprintln(os.Stderr, "                          requests over a Unix socket. Eliminates per-call")
	fmt.Fprintln(os.Stderr, "                          subprocess spawn for in-process clients.")
	fmt.Fprintln(os.Stderr, "  version                 Print version")
}

// openDB opens a Dolt database via the embedded driver.
func openDB(dataDir string) *sql.DB {
	// The dolthub/driver DSN: file:///path?commitname=...&commitemail=...&database=...
	// The database parameter selects the active database. Without it,
	// queries fail with "no database selected".
	dsn := fmt.Sprintf("file://%s?commitname=smooth&commitemail=pearls@smoo.ai&database=pearls", dataDir)
	db, err := sql.Open("dolt", dsn)
	if err != nil {
		fatal("open database: " + err.Error())
	}
	return db
}

func cmdInit(dataDir string) {
	if err := os.MkdirAll(dataDir, 0o755); err != nil {
		fatal("mkdir: " + err.Error())
	}

	// Open without selecting a database first, then CREATE DATABASE.
	dsn := fmt.Sprintf("file://%s?commitname=smooth&commitemail=pearls@smoo.ai", dataDir)
	db, err := sql.Open("dolt", dsn)
	if err != nil {
		fatal("init open: " + err.Error())
	}
	defer db.Close()

	// Ping to ensure the engine starts.
	if err := db.Ping(); err != nil {
		fatal("init ping: " + err.Error())
	}

	// Create the "pearls" database if it doesn't exist.
	if _, err := db.Exec("CREATE DATABASE IF NOT EXISTS pearls"); err != nil {
		fatal("create database: " + err.Error())
	}

	// Initial commit so the database has a root commit.
	if _, err := db.Exec("USE pearls"); err != nil {
		fatal("use pearls: " + err.Error())
	}
	if _, err := db.Exec("CALL DOLT_ADD('-A')"); err != nil {
		// May fail if nothing to add — that's fine.
		_ = err
	}
	if _, err := db.Exec("CALL DOLT_COMMIT('--allow-empty', '-m', 'initialize pearl database')"); err != nil {
		// May fail if already committed — that's fine.
		_ = err
	}

	fmt.Println("initialized dolt database at", dataDir)
}

func cmdSQL(dataDir string, query string) {
	db := openDB(dataDir)
	defer db.Close()

	rows, err := db.Query(query)
	if err != nil {
		fatal("sql: " + err.Error())
	}
	defer rows.Close()

	cols, err := rows.Columns()
	if err != nil {
		fatal("columns: " + err.Error())
	}

	var results []map[string]interface{}
	for rows.Next() {
		values := make([]interface{}, len(cols))
		ptrs := make([]interface{}, len(cols))
		for i := range values {
			ptrs[i] = &values[i]
		}
		if err := rows.Scan(ptrs...); err != nil {
			fatal("scan: " + err.Error())
		}
		row := make(map[string]interface{})
		for i, col := range cols {
			v := values[i]
			// Convert []byte to string for JSON output.
			if b, ok := v.([]byte); ok {
				row[col] = string(b)
			} else {
				row[col] = v
			}
		}
		results = append(results, row)
	}

	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(results); err != nil {
		fatal("json: " + err.Error())
	}
}

func cmdCommit(dataDir string, message string) {
	db := openDB(dataDir)
	defer db.Close()

	// Stage all changes.
	if _, err := db.Exec("CALL DOLT_ADD('-A')"); err != nil {
		fatal("dolt_add: " + err.Error())
	}

	// Commit.
	if _, err := db.Exec("CALL DOLT_COMMIT('-m', ?, '--allow-empty')", message); err != nil {
		fatal("dolt_commit: " + err.Error())
	}
	fmt.Println("committed:", message)
}

func cmdLog(dataDir string, n int) {
	db := openDB(dataDir)
	defer db.Close()

	query := fmt.Sprintf("SELECT commit_hash, committer, date, message FROM dolt_log LIMIT %d", n)
	rows, err := db.Query(query)
	if err != nil {
		fatal("dolt_log: " + err.Error())
	}
	defer rows.Close()

	for rows.Next() {
		var hash, author, date, msg string
		if err := rows.Scan(&hash, &author, &date, &msg); err != nil {
			fatal("scan: " + err.Error())
		}
		short := hash
		if len(short) > 8 {
			short = short[:8]
		}
		fmt.Printf("%s %s (%s) %s\n", short, msg, author, date)
	}
}

func cmdClone(remoteURL, dataDir string) {
	// Initialize locally, add remote, pull. Full clone requires deeper
	// integration with dolt's clone machinery; this two-step approach
	// works for the MVP.
	cmdInit(dataDir)
	db := openDB(dataDir)
	defer db.Close()
	if _, err := db.Exec("CALL DOLT_REMOTE('add', 'origin', ?)", remoteURL); err != nil {
		fatal("dolt_remote add: " + err.Error())
	}
	if _, err := db.Exec("CALL DOLT_PULL('origin', 'main')"); err != nil {
		fatal("dolt_pull: " + err.Error())
	}
	fmt.Println("cloned from", remoteURL, "to", dataDir)
}

func cmdRemote(dataDir string, args []string) {
	if len(args) == 0 {
		fatal("usage: smooth-dolt remote <data-dir> add|list ...")
	}
	db := openDB(dataDir)
	defer db.Close()

	switch args[0] {
	case "add":
		if len(args) < 3 {
			fatal("usage: smooth-dolt remote <data-dir> add <name> <url>")
		}
		if _, err := db.Exec("CALL DOLT_REMOTE('add', ?, ?)", args[1], args[2]); err != nil {
			fatal("dolt_remote add: " + err.Error())
		}
		fmt.Printf("added remote %s → %s\n", args[1], args[2])
	case "list":
		rows, err := db.Query("SELECT name, url FROM dolt_remotes")
		if err != nil {
			fatal("dolt_remotes: " + err.Error())
		}
		defer rows.Close()
		for rows.Next() {
			var name, url string
			if err := rows.Scan(&name, &url); err != nil {
				continue
			}
			fmt.Printf("%s\t%s\n", name, url)
		}
	case "remove":
		if len(args) < 2 {
			fatal("usage: smooth-dolt remote <data-dir> remove <name>")
		}
		if _, err := db.Exec("CALL DOLT_REMOTE('remove', ?)", args[1]); err != nil {
			fatal("dolt_remote remove: " + err.Error())
		}
		fmt.Printf("removed remote %s\n", args[1])
	default:
		fatal("unknown remote subcommand: " + args[0])
	}
}

func cmdDoltCmd(dataDir string, doltCmd string) {
	db := openDB(dataDir)
	defer db.Close()

	callSQL := fmt.Sprintf("CALL DOLT_%s()", strings.ToUpper(doltCmd))
	if _, err := db.Exec(callSQL); err != nil {
		fatal(doltCmd + ": " + err.Error())
	}
	fmt.Println(doltCmd + ": ok")
}

func findFlag(args []string, flag string) string {
	for i, a := range args {
		if a == flag && i+1 < len(args) {
			return args[i+1]
		}
	}
	return ""
}

func fatal(msg string) {
	fmt.Fprintln(os.Stderr, "smooth-dolt:", msg)
	os.Exit(1)
}

// ── serve subcommand ──────────────────────────────────────────────
//
// Long-running server. Opens the Dolt DB once and listens on a Unix
// domain socket for JSON-line requests. Each connection is handled
// independently and may issue many requests; one DB instance is shared
// across all of them. Exits on SIGTERM/SIGINT or when the listener
// is closed.
//
// Wire format (newline-delimited JSON):
//   request:  {"id":"<corr>","op":"sql","query":"..."}
//             {"id":"<corr>","op":"exec","stmt":"..."}
//             {"id":"<corr>","op":"commit","message":"..."}
//             {"id":"<corr>","op":"log","limit":20}
//             {"id":"<corr>","op":"dolt","cmd":"push|pull|gc|status"}
//             {"id":"<corr>","op":"ping"}
//   response: {"id":"<corr>","ok":true,"data":[...]}      // sql
//             {"id":"<corr>","ok":true,"rows_affected":1} // exec/commit
//             {"id":"<corr>","ok":true,"out":"...stdout..."} // dolt
//             {"id":"<corr>","ok":false,"error":"..."}    // any failure

type serveRequest struct {
	ID      string `json:"id"`
	Op      string `json:"op"`
	Query   string `json:"query,omitempty"`
	Stmt    string `json:"stmt,omitempty"`
	Message string `json:"message,omitempty"`
	Limit   int    `json:"limit,omitempty"`
	Cmd     string `json:"cmd,omitempty"`
}

type serveResponse struct {
	ID           string                   `json:"id,omitempty"`
	OK           bool                     `json:"ok"`
	Error        string                   `json:"error,omitempty"`
	Data         []map[string]interface{} `json:"data,omitempty"`
	Out          string                   `json:"out,omitempty"`
	RowsAffected int64                    `json:"rows_affected,omitempty"`
}

func cmdServe(dataDir, socketPath string) {
	// Best-effort cleanup of any stale socket from a previous run.
	_ = os.Remove(socketPath)

	listener, err := net.Listen("unix", socketPath)
	if err != nil {
		fatal("listen unix: " + err.Error())
	}
	// Restrict socket to owner only — pearl data is sensitive.
	if err := os.Chmod(socketPath, 0o600); err != nil {
		_ = listener.Close()
		fatal("chmod socket: " + err.Error())
	}
	defer func() { _ = os.Remove(socketPath) }()

	db := openDB(dataDir)
	defer db.Close()

	// Serialize all DB access. The dolthub/driver embedded engine is
	// not safe for concurrent use across goroutines for the kinds of
	// CALL-DOLT_* mutations we run, and the perf cost of a mutex is
	// negligible relative to query latency. Concurrent connections still
	// work — they just queue at the DB layer.
	var dbMu sync.Mutex

	// Trap signals so we can clean up the socket file.
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigCh
		_ = listener.Close()
	}()

	fmt.Fprintf(os.Stderr, "smooth-dolt: serve %s on %s\n", dataDir, socketPath)

	for {
		conn, err := listener.Accept()
		if err != nil {
			// Listener closed via signal handler — clean exit.
			return
		}
		go handleServeConn(conn, db, &dbMu)
	}
}

func handleServeConn(conn net.Conn, db *sql.DB, dbMu *sync.Mutex) {
	defer conn.Close()
	scanner := bufio.NewScanner(conn)
	// Pearl descriptions can be long; allow ~16MB request lines.
	scanner.Buffer(make([]byte, 64*1024), 16*1024*1024)
	encoder := json.NewEncoder(conn)

	for scanner.Scan() {
		line := scanner.Bytes()
		var req serveRequest
		if err := json.Unmarshal(line, &req); err != nil {
			_ = encoder.Encode(serveResponse{OK: false, Error: "parse request: " + err.Error()})
			continue
		}

		resp := dispatchServeReq(db, dbMu, &req)
		if err := encoder.Encode(resp); err != nil {
			// Client gone — drop this connection.
			return
		}
	}
}

func dispatchServeReq(db *sql.DB, dbMu *sync.Mutex, req *serveRequest) serveResponse {
	switch req.Op {
	case "ping":
		return serveResponse{ID: req.ID, OK: true}
	case "sql":
		return doSQL(db, dbMu, req.ID, req.Query)
	case "exec":
		return doExec(db, dbMu, req.ID, req.Stmt)
	case "commit":
		return doCommit(db, dbMu, req.ID, req.Message)
	case "log":
		limit := req.Limit
		if limit <= 0 {
			limit = 20
		}
		return doLog(db, dbMu, req.ID, limit)
	case "dolt":
		return doDoltCmd(db, dbMu, req.ID, req.Cmd)
	default:
		return serveResponse{ID: req.ID, OK: false, Error: "unknown op: " + req.Op}
	}
}

func doSQL(db *sql.DB, dbMu *sync.Mutex, id, query string) serveResponse {
	dbMu.Lock()
	defer dbMu.Unlock()

	rows, err := db.Query(query)
	if err != nil {
		return serveResponse{ID: id, OK: false, Error: "sql: " + err.Error()}
	}
	defer rows.Close()

	cols, err := rows.Columns()
	if err != nil {
		return serveResponse{ID: id, OK: false, Error: "columns: " + err.Error()}
	}

	results := []map[string]interface{}{}
	for rows.Next() {
		values := make([]interface{}, len(cols))
		ptrs := make([]interface{}, len(cols))
		for i := range values {
			ptrs[i] = &values[i]
		}
		if err := rows.Scan(ptrs...); err != nil {
			return serveResponse{ID: id, OK: false, Error: "scan: " + err.Error()}
		}
		row := make(map[string]interface{}, len(cols))
		for i, col := range cols {
			v := values[i]
			if b, ok := v.([]byte); ok {
				row[col] = string(b)
			} else {
				row[col] = v
			}
		}
		results = append(results, row)
	}
	return serveResponse{ID: id, OK: true, Data: results}
}

func doExec(db *sql.DB, dbMu *sync.Mutex, id, stmt string) serveResponse {
	dbMu.Lock()
	defer dbMu.Unlock()

	res, err := db.Exec(stmt)
	if err != nil {
		return serveResponse{ID: id, OK: false, Error: "exec: " + err.Error()}
	}
	rows, _ := res.RowsAffected()
	return serveResponse{ID: id, OK: true, RowsAffected: rows}
}

func doCommit(db *sql.DB, dbMu *sync.Mutex, id, message string) serveResponse {
	dbMu.Lock()
	defer dbMu.Unlock()

	if _, err := db.Exec("CALL DOLT_ADD('-A')"); err != nil {
		return serveResponse{ID: id, OK: false, Error: "dolt_add: " + err.Error()}
	}
	if _, err := db.Exec("CALL DOLT_COMMIT('-m', ?, '--allow-empty')", message); err != nil {
		return serveResponse{ID: id, OK: false, Error: "dolt_commit: " + err.Error()}
	}
	return serveResponse{ID: id, OK: true, Out: "committed: " + message}
}

func doLog(db *sql.DB, dbMu *sync.Mutex, id string, n int) serveResponse {
	dbMu.Lock()
	defer dbMu.Unlock()

	query := fmt.Sprintf("SELECT commit_hash, committer, date, message FROM dolt_log LIMIT %d", n)
	rows, err := db.Query(query)
	if err != nil {
		return serveResponse{ID: id, OK: false, Error: "dolt_log: " + err.Error()}
	}
	defer rows.Close()

	var lines []string
	for rows.Next() {
		var hash, author, date, msg string
		if err := rows.Scan(&hash, &author, &date, &msg); err != nil {
			continue
		}
		short := hash
		if len(short) > 8 {
			short = short[:8]
		}
		lines = append(lines, fmt.Sprintf("%s %s (%s) %s", short, msg, author, date))
	}
	return serveResponse{ID: id, OK: true, Out: strings.Join(lines, "\n")}
}

func doDoltCmd(db *sql.DB, dbMu *sync.Mutex, id, cmd string) serveResponse {
	dbMu.Lock()
	defer dbMu.Unlock()

	switch cmd {
	case "push", "pull", "gc", "status":
		// Allowed; fall through.
	default:
		return serveResponse{ID: id, OK: false, Error: "unsupported dolt cmd: " + cmd}
	}

	callSQL := fmt.Sprintf("CALL DOLT_%s()", strings.ToUpper(cmd))
	if _, err := db.Exec(callSQL); err != nil {
		return serveResponse{ID: id, OK: false, Error: cmd + ": " + err.Error()}
	}
	return serveResponse{ID: id, OK: true, Out: cmd + ": ok"}
}
