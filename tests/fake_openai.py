#!/usr/bin/env python3
"""Tiny OpenAI-compatible chat server for tests. Replies with the transcript upper-cased and logs the Authorization header."""
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        user = body["messages"][-1]["content"]
        text = user.split("<<<\n", 1)[1].rsplit("\n>>>", 1)[0] if "<<<" in user else user
        print(f"path={self.path} auth={self.headers.get('Authorization')} model={body['model']} sys={body['messages'][0]['role']}", flush=True)
        out = json.dumps({"choices": [{"message": {"role": "assistant", "content": text.upper()}}]}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(out)))
        self.end_headers(); self.wfile.write(out)
    def log_message(self, *a): pass

HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
