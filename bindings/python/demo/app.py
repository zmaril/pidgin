"""Minimal Flask chat demo for the pidgin Python native extension.

Round-trips a message through `pidgin.Session` into the real agent loop and
appends the assistant reply to a transcript.

Provider selection:
- Offline **faux** by default (no network, no API key) — the supported path.
- **Live** Anthropic when ``ANTHROPIC_API_KEY`` is set in the environment; the
  model comes from ``PIDGIN_MODEL`` (default ``claude-sonnet-4-5``).

Session strategy: a single global ``pidgin.Session`` shared by the process, with
a lock serializing turns. This keeps the demo tiny; the engine's own worker
thread already serializes execution. For per-user isolation you would key a
Session by the Flask session cookie instead — noted here as the deliberate
simplification it is.
"""

import os
import threading

from flask import Flask, jsonify, render_template, request

import pidgin

app = Flask(__name__)

_USE_LIVE = bool(os.environ.get("ANTHROPIC_API_KEY"))
_MODEL = os.environ.get("PIDGIN_MODEL", "claude-sonnet-4-5")

# One shared session for the whole process; a lock serializes turns.
_lock = threading.Lock()
if _USE_LIVE:
    _session = pidgin.Session(faux=False, model=_MODEL)
    _MODE = f"live ({_MODEL})"
else:
    _session = pidgin.Session(faux=True)
    _MODE = "faux (offline)"

# In-memory transcript: list of {"role": "user"|"assistant", "text": str}.
_transcript = []


@app.route("/")
def index():
    return render_template("index.html", transcript=_transcript, mode=_MODE)


@app.route("/send", methods=["POST"])
def send():
    message = (request.form.get("message") or request.json.get("message", "")).strip()
    if not message:
        return jsonify({"error": "empty message"}), 400

    with _lock:
        reply = _session.send(message)
        _transcript.append({"role": "user", "text": message})
        _transcript.append({"role": "assistant", "text": reply})

    return jsonify({"reply": reply})


if __name__ == "__main__":
    app.run(debug=True)
