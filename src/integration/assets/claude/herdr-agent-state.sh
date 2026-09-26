#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=11

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-claude-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session|subagent_start|subagent_stop) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_ACTION="$action" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:claude"
action = os.environ.get("HERDR_ACTION", "")
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
hook_input_file = os.environ.get("HERDR_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}

if "CURSOR_VERSION" in os.environ or "cursor_version" in hook_input:
    raise SystemExit(0)
hook_event_name = str(hook_input.get("hook_event_name") or "")


def clean_str(value):
    return value if isinstance(value, str) and value else None


def build_request():
    request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
    if action == "session":
        return build_session_request(request_id)
    if action == "subagent_start":
        return build_subagent_start_request(request_id)
    if action == "subagent_stop":
        return build_subagent_stop_request(request_id)
    return None


def build_session_request(request_id):
    if hook_event_name != "SessionStart":
        return None
    if hook_input.get("agent_id"):
        return None
    agent_session_id = clean_str(hook_input.get("session_id"))
    agent_session_path = clean_str(hook_input.get("transcript_path"))
    session_start_source = clean_str(hook_input.get("source"))
    if not agent_session_id:
        return None
    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": "claude",
        "seq": time.time_ns(),
        "agent_session_id": agent_session_id,
    }
    if agent_session_path:
        params["agent_session_path"] = agent_session_path
    if session_start_source:
        params["session_start_source"] = session_start_source
    return {
        "id": request_id,
        "method": "pane.report_agent_session",
        "params": params,
    }


def subagent_fields():
    agent_id = clean_str(hook_input.get("agent_id"))
    agent_type = clean_str(hook_input.get("agent_type")) or "unknown"
    return agent_id, agent_type


def build_subagent_start_request(request_id):
    if hook_event_name != "SubagentStart":
        return None
    agent_id, agent_type = subagent_fields()
    if not agent_id:
        return None
    transcript_path = None
    main_transcript = clean_str(hook_input.get("transcript_path"))
    session_id = clean_str(hook_input.get("session_id"))
    if main_transcript and session_id:
        main_transcript = os.path.expanduser(main_transcript)
        candidate = os.path.join(
            os.path.dirname(main_transcript),
            session_id,
            "subagents",
            f"agent-{agent_id}.jsonl",
        )
        if os.path.isabs(candidate):
            transcript_path = candidate
    return {
        "id": request_id,
        "method": "pane.report_agent_subagent",
        "params": {
            "pane_id": pane_id,
            "source": source,
            "agent": "claude",
            "event": "start",
            "agent_id": agent_id,
            "agent_type": agent_type,
            "seq": time.time_ns(),
            **({"transcript_path": transcript_path} if transcript_path else {}),
        },
    }


def build_subagent_stop_request(request_id):
    if hook_event_name != "SubagentStop":
        return None
    agent_id, agent_type = subagent_fields()
    if not agent_id:
        return None
    agent_transcript = clean_str(hook_input.get("agent_transcript_path"))
    if agent_transcript:
        agent_transcript = os.path.expanduser(agent_transcript)
    last_message = clean_str(hook_input.get("last_assistant_message"))
    if last_message:
        last_message = last_message.replace("\n", " ").replace("\r", " ")[:500].strip()
    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": "claude",
        "event": "stop",
        "agent_id": agent_id,
        "agent_type": agent_type,
        "seq": time.time_ns(),
    }
    if agent_transcript:
        params["transcript_path"] = agent_transcript
    if last_message:
        params["last_assistant_message"] = last_message
    return {
        "id": request_id,
        "method": "pane.report_agent_subagent",
        "params": params,
    }


request = build_request()
if not request:
    raise SystemExit(0)

try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
