#!/usr/bin/env python3
"""A local OpenAI-compatible mock for tests/scrape_live.rs (th-e77603).

Lets a hookless coding CLI reach a real working -> idle turn, a real tool
approval and a real edit question with NO account, NO API key and NO model.
It streams slowly so a capture can land mid-turn.

    python3 mock_openai.py 18777        # then point the CLI at http://127.0.0.1:18777/v1

The reply is chosen by the last user message:
  contains "newfile" -> an aider whole-file edit for a new file ("Create new file?")
  contains "tool"    -> one call to the first shell-like tool the client offers
  otherwise          -> STREAM_SECS of prose, after PRE_SECS of silence

Env: STREAM_SECS (8), PRE_SECS (4), MOCK_CMD (the tool call's command).
Standard library only.
"""
import json
import os
import sys
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18777
SECS = float(os.environ.get("STREAM_SECS", "8"))
PRE = float(os.environ.get("PRE_SECS", "4"))
CMD = os.environ.get("MOCK_CMD", "mkdir -p mockdir && touch mockdir/x.txt")
PROSE = ("Sure. Here is a deliberately slow answer so the terminal stays busy for a while. " * 6).split(" ")


def chunk(model, delta, finish=None):
    return {
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": int(time.time()),
        "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    }


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

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            return self._json({"object": "list", "data": [{"id": "mock-model", "object": "model", "owned_by": "mock"}]})
        return self._json({"ok": True})

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        req = json.loads(self.rfile.read(length) or b"{}")
        model = req.get("model", "mock-model")
        msgs = req.get("messages") or []
        last = ""
        for m in reversed(msgs):
            if m.get("role") == "user":
                c = m.get("content")
                last = c if isinstance(c, str) else json.dumps(c)
                break
        after_tool = bool(msgs) and msgs[-1].get("role") == "tool"
        tool = None
        if "newfile" in last.lower() and not after_tool:
            words = ["hello.py\n", "```python\n", "print('hello')\n", "```\n"]
        elif "tool" in last.lower() and not after_tool and req.get("tools"):
            names = [t.get("function", t).get("name") for t in req["tools"]]
            shell = [n for n in names if any(k in n for k in ("bash", "shell", "command"))]
            words, tool = [], (shell or names)[0]
        else:
            words = PROSE
        if not req.get("stream"):
            text = "".join(w + " " for w in words)
            return self._json(
                {
                    "id": "x",
                    "object": "chat.completion",
                    "model": model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                }
            )
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()

        def send(obj):
            self.wfile.write(("data: " + json.dumps(obj) + "\n\n").encode())
            self.wfile.flush()

        time.sleep(PRE)
        send(chunk(model, {"role": "assistant", "content": ""}))
        if tool:
            time.sleep(SECS / 2)
            args = json.dumps({"command": CMD, "description": "print a marker", "requires_approval": True})
            call = {"index": 0, "id": "call_" + uuid.uuid4().hex[:8], "type": "function", "function": {"name": tool, "arguments": args}}
            send(chunk(model, {"tool_calls": [call]}))
            send(chunk(model, {}, "tool_calls"))
        else:
            gap = SECS / max(1, len(words))
            for w in words:
                time.sleep(gap)
                send(chunk(model, {"content": w + ("" if w.endswith("\n") else " ")}))
            send(chunk(model, {}, "stop"))
        send({"id": "x", "object": "chat.completion.chunk", "model": model, "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20}})
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        self.close_connection = True


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
