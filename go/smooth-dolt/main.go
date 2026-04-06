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
//	smooth-dolt version

package main

import (
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"

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
