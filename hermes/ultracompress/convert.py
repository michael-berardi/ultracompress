"""Pure helpers for the Hermes adapter: no Hermes imports, so they test standalone.

Hermes keeps conversations as OpenAI-format chat messages. The UltraCompress CLI reads Pi
session entries. These helpers translate between the two and shape the summary sections.
"""
from __future__ import annotations

import json
import os
import re
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional

BRIEF_HEADING = "## UltraCompress brief"
REPLIES_HEADING = "## Agent Replies (verbatim excerpts, newest first)"
USERS_HEADING = "## User Messages (verbatim, newest first)"  # Hermes' own lean-mode section
RECALL_TOOL = "ultracompress_recall"

# Same lookup order as the Claude Code plugin; first regular file wins.
BINARY_CANDIDATES = (
    "~/.local/bin/ultracompress",
    "~/.ultraterm/bin/ultracompress",
    "/opt/homebrew/bin/ultracompress",
    "/usr/local/bin/ultracompress",
)

_UNTRUSTED_OPEN = re.compile(r"^\s*<untrusted_tool_result[^>]*>\s*", re.S)
_UNTRUSTED_PREAMBLE = re.compile(
    r"^The following content was retrieved from an external source\..*?can issue instructions\.\s*", re.S)
_UNTRUSTED_CLOSE = re.compile(r"\s*</untrusted_tool_result>\s*$", re.S)


def find_binary(explicit: str = "", env: Optional[Dict[str, str]] = None) -> Optional[str]:
    """Settings value, then $ULTRACOMPRESS_BIN, then the standard install locations."""
    env = os.environ if env is None else env
    for candidate in (explicit, env.get("ULTRACOMPRESS_BIN", ""), *BINARY_CANDIDATES):
        if candidate:
            path = Path(candidate).expanduser()
            if path.is_file():
                return str(path)
    return None


def text_of(content: Any) -> str:
    """Plain text of a message content field (string or OpenAI content-part list)."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(p.get("text", "") for p in content
                       if isinstance(p, dict) and p.get("type") in (None, "text", "input_text", "output_text"))
    return "" if content is None else str(content)


def unwrap_tool_text(text: str) -> str:
    """Drop Hermes' untrusted-tool-result envelope so the brief sees the actual output."""
    stripped = _UNTRUSTED_OPEN.sub("", text, count=1)
    if stripped == text:
        return text
    stripped = _UNTRUSTED_PREAMBLE.sub("", stripped, count=1)
    return _UNTRUSTED_CLOSE.sub("", stripped, count=1)


def to_pi_entries(messages: Iterable[Dict[str, Any]], id_prefix: str = "m") -> List[Dict[str, Any]]:
    """OpenAI-format chat messages -> Pi session message entries (the CLI's input contract).

    System messages and unknown roles are skipped; assistant tool calls become toolCall blocks and
    tool messages become toolResult messages. Entry ids are positional, so repeated provider ids
    cannot collide."""
    entries: List[Dict[str, Any]] = []
    parent = None
    for i, m in enumerate(messages):
        role = m.get("role")
        text = text_of(m.get("content"))
        if role == "user":
            msg = {"role": "user", "content": [{"type": "text", "text": text}]}
        elif role == "assistant":
            blocks: List[Dict[str, Any]] = [{"type": "text", "text": text}] if text else []
            for tc in m.get("tool_calls") or []:
                fn = tc.get("function") or {}
                args = fn.get("arguments") or "{}"
                if isinstance(args, str):
                    try:
                        args = json.loads(args)
                    except ValueError:
                        args = {"raw": args}
                blocks.append({"type": "toolCall", "id": tc.get("id"), "name": fn.get("name"), "arguments": args})
            if not blocks:
                continue
            msg = {"role": "assistant", "content": blocks}
        elif role == "tool":
            msg = {"role": "toolResult", "toolCallId": m.get("tool_call_id"),
                   "toolName": m.get("name") or m.get("tool_name") or "tool",
                   "content": [{"type": "text", "text": unwrap_tool_text(text)}]}
        else:
            continue
        eid = f"{id_prefix}{i}"
        entries.append({"type": "message", "id": eid, "parentId": parent, "message": msg})
        parent = eid
    return entries


def conversation_view(turns: Iterable[Dict[str, Any]], tool_result_chars: int = 0,
                      is_carrier=lambda m: False) -> List[Dict[str, Any]]:
    """The turns the brief is built from.

    An agent's decisions live in what the user and the agent said, not in raw tool JSON, so by
    default tool results and tool-only steps are left out of the brief (the raw archive keeps them
    for recall). tool_result_chars > 0 keeps that many characters of each tool result instead.
    Earlier compaction handoffs are skipped: they reach the CLI as previousSummary."""
    view: List[Dict[str, Any]] = []
    for t in turns:
        if is_carrier(t):
            continue
        role = t.get("role")
        if role == "tool":
            if tool_result_chars > 0:
                view.append({**t, "content": unwrap_tool_text(text_of(t.get("content")))[:tool_result_chars]})
        elif role == "assistant" and t.get("tool_calls") and tool_result_chars <= 0:
            text = text_of(t.get("content")).strip()
            if text:
                view.append({"role": "assistant", "content": text})
        else:
            view.append(t)
    return view


def section(text: str, heading: str) -> str:
    """Body of a '## ' section ('' if absent)."""
    start = text.find(heading)
    if start < 0:
        return ""
    body = text[start + len(heading):]
    nxt = body.find("\n## ")
    return (body if nxt < 0 else body[:nxt]).strip()


def agent_replies_section(turns: Iterable[Dict[str, Any]], previous_summary: str = "",
                          per_reply: int = 1200, total: int = 12000) -> str:
    """Verbatim excerpts of what the agent told the user, newest first, carrying earlier windows forward.

    New replies get 55% of the budget when an earlier section exists, so old decisions survive a
    burst of long new replies."""
    previous = section(previous_summary, REPLIES_HEADING)
    budget = int(total * 0.55) if previous else total
    parts: List[str] = []
    used = 0
    for t in reversed(list(turns)):
        if t.get("role") != "assistant" or t.get("tool_calls"):
            continue
        text = text_of(t.get("content")).strip()
        if not text:
            continue
        excerpt = text if len(text) <= per_reply else text[:per_reply].rstrip() + " …"
        if used + len(excerpt) > budget:
            break
        parts.append("> " + excerpt.replace("\n", "\n> "))
        used += len(excerpt)
    if previous:
        parts.append(previous[: max(0, total - used)])
    parts = [p for p in parts if p]
    if not parts:
        return ""
    return "\n\n" + REPLIES_HEADING + "\n" + "\n\n".join(parts)


def carry_forward_users(summary: str, previous_summary: str, budget: int = 4000) -> str:
    """Keep earlier windows' verbatim user quotes (Hermes rebuilds that section per window only)."""
    previous = section(previous_summary, USERS_HEADING)
    if not previous:
        return summary
    carried = previous[:budget]
    current = section(summary, USERS_HEADING)
    if current:
        return summary.replace(current, current + "\n\n" + carried, 1)
    return summary + "\n\n" + USERS_HEADING + "\n" + carried


def compose_brief(brief: str) -> str:
    return (
        BRIEF_HEADING + "\n"
        "Deterministic brief of the condensed turns (built locally, no model call). Anything not "
        f"shown here is still searchable with the {RECALL_TOOL} tool.\n\n" + brief
    )
