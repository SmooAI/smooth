#!/usr/bin/env python3
"""A local mock of the three model APIs the fleet's main harnesses speak (SMOODEV-4268).

Claude Code (`POST /v1/messages`), Codex (`POST /v1/responses`) and OpenCode
(`POST /v1/chat/completions`) reach a real turn, a real tool approval and a real
question with NO account, NO key and NO model. It streams slowly so the status
rig (`rig.py`) can see `working`.

The reply is chosen by the last user message:
  contains "rig-tool"     -> one call to the client's shell tool (needs approval)
  contains "rig-question" -> one call to the client's ask-the-user tool
  contains "rig-slow"     -> SLOW_SECS of prose
  otherwise               -> STREAM_SECS of prose
A message that is a tool result gets a short closing sentence.

Env: STREAM_SECS (4), SLOW_SECS (20), PRE_SECS (1), MOCK_CMD. Standard library only.
"""
import json
import os
import sys
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18788
STREAM = float(os.environ.get("STREAM_SECS", "4"))
SLOW = float(os.environ.get("SLOW_SECS", "20"))
PRE = float(os.environ.get("PRE_SECS", "1"))
CMD = os.environ.get("MOCK_CMD", "mkdir -p mockdir && touch mockdir/x.txt")
QUESTION = "Which color should the rig use?"
WORDS = ("Sure. Here is a deliberately slow answer so the terminal stays busy for a while. " * 4).split(" ")
LOG = os.environ.get("MOCK_LOG")


def log(msg):
    if LOG:
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(msg + "\n")


def text_of(content):
    if isinstance(content, str):
        return content
    return json.dumps(content)


def pick(last_text, after_tool, names):
    """(kind, tool name): kind is `tool`, `question`, `slow`, `short` or `prose`."""
    lower = last_text.lower()
    if after_tool:
        return "short", None
    if "rig-tool" in lower and names:
        shell = [n for n in names if n.lower() in ("bash", "shell", "exec_command", "shell_command", "local_shell")]
        return ("tool", shell[0]) if shell else ("prose", None)
    if "rig-question" in lower and names:
        ask = [n for n in names if n.lower() in ("askuserquestion", "question", "request_user_input")]
        return ("question", ask[0]) if ask else ("prose", None)
    if "rig-slow" in lower:
        return "slow", None
    return "prose", None


def question_args(name):
    option = [{"label": "Teal", "description": "the teal one"}, {"label": "Amber", "description": "the amber one"}]
    if name.lower() == "request_user_input":
        return {"questions": [{"id": "color", "header": "Color", "question": QUESTION, "options": option}]}
    return {"questions": [{"question": QUESTION, "header": "Color", "options": option, "multiSelect": False}]}


def shell_args(name):
    # Codex's sandboxed tools run unasked unless the call requests escalation.
    escalate = {"sandbox_permissions": "require_escalated", "justification": "the rig needs an approval"}
    if name.lower() == "exec_command":
        return {"cmd": CMD, **escalate}
    if name.lower() in ("shell", "local_shell"):
        return {"command": ["bash", "-lc", CMD], **escalate}
    if name.lower() == "shell_command":
        return {"command": CMD, **escalate}
    return {"command": CMD, "description": "make a marker directory"}


def tool_call(kind, name):
    return question_args(name) if kind == "question" else shell_args(name)


def prose_for(kind):
    if kind == "short":
        return ["Done."], 0.5
    return WORDS, SLOW if kind == "slow" else STREAM


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _json(self, obj):
        body = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _sse_open(self):
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        self.close_connection = True

    def _event(self, name, obj):
        prefix = f"event: {name}\n" if name else ""
        self.wfile.write((prefix + "data: " + json.dumps(obj) + "\n\n").encode())
        self.wfile.flush()

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models") or "/models?" in self.path:
            return self._json(
                {
                    "object": "list",
                    "data": [{"id": "mock-model", "object": "model", "owned_by": "mock"}],
                    "models": [],
                }
            )
        return self._json({"ok": True})

    def do_HEAD(self):
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        req = json.loads(self.rfile.read(length) or b"{}")
        path = self.path.split("?")[0]
        try:
            if path.endswith("/messages/count_tokens"):
                return self._json({"input_tokens": 10})
            if path.endswith("/messages"):
                return self._anthropic(req)
            if path.endswith("/responses"):
                return self._responses(req)
            if path.endswith("/chat/completions"):
                return self._chat(req)
        except (BrokenPipeError, ConnectionResetError):
            return None
        return self._json({"ok": True})

    # ── Anthropic Messages (Claude Code) ────────────────────────────────────
    def _anthropic(self, req):
        msgs = req.get("messages") or []
        last = msgs[-1] if msgs else {}
        content = last.get("content")
        after_tool = isinstance(content, list) and any(isinstance(c, dict) and c.get("type") == "tool_result" for c in content)
        # A denied call's result arrives in the same message as the next prompt.
        if after_tool and any(isinstance(c, dict) and c.get("type") == "text" and "rig-" in str(c.get("text")) for c in content):
            after_tool = False
        names = [t.get("name", "") for t in req.get("tools") or []]
        kind, tool = pick(text_of(content), after_tool, names)
        log(f"anthropic kind={kind} tool={tool} tools={names[:40]}")
        model = req.get("model", "mock-model")
        if not req.get("stream"):
            return self._json(
                {
                    "id": "msg_mock",
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [{"type": "text", "text": "Mock title"}],
                    "stop_reason": "end_turn",
                    "stop_sequence": None,
                    "usage": {"input_tokens": 1, "output_tokens": 1},
                }
            )
        self._sse_open()
        start = {
            "type": "message_start",
            "message": {
                "id": "msg_" + uuid.uuid4().hex[:12],
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": [],
                "stop_reason": None,
                "stop_sequence": None,
                "usage": {"input_tokens": 10, "output_tokens": 1},
            },
        }
        time.sleep(PRE)
        self._event("message_start", start)
        if kind in ("tool", "question"):
            block = {"type": "tool_use", "id": "toolu_" + uuid.uuid4().hex[:16], "name": tool, "input": {}}
            self._event("content_block_start", {"type": "content_block_start", "index": 0, "content_block": block})
            delta = {"type": "input_json_delta", "partial_json": json.dumps(tool_call(kind, tool))}
            self._event("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": delta})
            stop = "tool_use"
        else:
            self._event("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
            words, secs = prose_for(kind)
            for w in words:
                time.sleep(secs / max(1, len(words)))
                self._event("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": w + " "}})
            stop = "end_turn"
        self._event("content_block_stop", {"type": "content_block_stop", "index": 0})
        self._event("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": 5}})
        self._event("message_stop", {"type": "message_stop"})
        return None

    # ── OpenAI Responses (Codex) ────────────────────────────────────────────
    def _responses(self, req):
        items = req.get("input") or []
        last_user = ""
        after_tool = False
        if items and isinstance(items, list):
            tail = items[-1]
            after_tool = tail.get("type") in ("function_call_output", "custom_tool_call_output", "local_shell_call_output")
            for it in reversed(items):
                if it.get("role") == "user":
                    last_user = text_of(it.get("content"))
                    break
        names = [t.get("name", t.get("type", "")) for t in req.get("tools") or []]
        kind, tool = pick(last_user, after_tool, names)
        log(f"responses kind={kind} tool={tool} tools={names[:40]}")
        rid = "resp_" + uuid.uuid4().hex[:12]
        self._sse_open()
        time.sleep(PRE)
        self._event("response.created", {"type": "response.created", "response": {"id": rid}})
        if kind in ("tool", "question"):
            item = {
                "type": "function_call",
                "id": "fc_" + uuid.uuid4().hex[:8],
                "call_id": "call_" + uuid.uuid4().hex[:8],
                "name": tool,
                "arguments": json.dumps(tool_call(kind, tool)),
                "status": "completed",
            }
            self._event("response.output_item.done", {"type": "response.output_item.done", "output_index": 0, "item": item})
        else:
            words, secs = prose_for(kind)
            mid = "msg_" + uuid.uuid4().hex[:8]
            added = {"type": "message", "id": mid, "role": "assistant", "status": "in_progress", "content": []}
            self._event("response.output_item.added", {"type": "response.output_item.added", "output_index": 0, "item": added})
            for w in words:
                time.sleep(secs / max(1, len(words)))
                self._event("response.output_text.delta", {"type": "response.output_text.delta", "item_id": mid, "output_index": 0, "content_index": 0, "delta": w + " "})
            full = " ".join(words)
            item = {"type": "message", "id": mid, "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": full, "annotations": []}]}
            self._event("response.output_item.done", {"type": "response.output_item.done", "output_index": 0, "item": item})
        usage = {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 5, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 15}
        self._event("response.completed", {"type": "response.completed", "response": {"id": rid, "usage": usage}})
        return None

    # ── OpenAI Chat Completions (OpenCode) ─────────────────────────────────
    def _chat(self, req):
        msgs = req.get("messages") or []
        last = ""
        for m in reversed(msgs):
            if m.get("role") == "user":
                last = text_of(m.get("content"))
                break
        after_tool = bool(msgs) and msgs[-1].get("role") == "tool"
        names = [t.get("function", t).get("name", "") for t in req.get("tools") or []]
        kind, tool = pick(last, after_tool, names)
        log(f"chat kind={kind} tool={tool} tools={names[:40]}")
        model = req.get("model", "mock-model")
        if not req.get("stream"):
            return self._json(
                {
                    "id": "x",
                    "object": "chat.completion",
                    "model": model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "Mock title"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                }
            )

        def chunk(delta, finish=None):
            return {
                "id": "chatcmpl-mock",
                "object": "chat.completion.chunk",
                "created": int(time.time()),
                "model": model,
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            }

        self._sse_open()
        time.sleep(PRE)
        self._event(None, chunk({"role": "assistant", "content": ""}))
        if kind in ("tool", "question"):
            call = {"index": 0, "id": "call_" + uuid.uuid4().hex[:8], "type": "function", "function": {"name": tool, "arguments": json.dumps(tool_call(kind, tool))}}
            self._event(None, chunk({"tool_calls": [call]}))
            self._event(None, chunk({}, "tool_calls"))
        else:
            words, secs = prose_for(kind)
            for w in words:
                time.sleep(secs / max(1, len(words)))
                self._event(None, chunk({"content": w + " "}))
            self._event(None, chunk({}, "stop"))
        self._event(None, {"id": "x", "object": "chat.completion.chunk", "model": model, "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}})
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        return None


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
