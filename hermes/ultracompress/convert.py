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

# The brief keeps only the tool calls that carry a fact the next window needs: a file that was
# written or edited, and a commit that was made. Harnesses name those operations differently, so
# they are mapped to the CLI's edit/write/bash handling before they reach it.
TOOL_NAME_ALIASES = {
    "write_file": "write",
    "patch": "edit",
    "str_replace_editor": "edit",
    "terminal": "bash",
    "shell": "bash",
    "execute": "bash",
}
WRITE_TOOL_NAMES = frozenset({"write", "write_file", "create_file", "make_file"})
EDIT_TOOL_NAMES = frozenset({"edit", "patch", "apply_patch", "str_replace", "multi_edit"})
SHELL_TOOL_NAMES = frozenset({"bash", "shell", "terminal", "execute", "run_command"})
COMMIT_MARKER = "git commit"
TOOL_ARGUMENT_CHARS = 400

# Same lookup order as the Claude Code plugin; first executable regular file wins.
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
    """Settings value, then $ULTRACOMPRESS_BIN, then the standard install locations.

    The file must be executable: an interrupted download left without the bit would otherwise turn
    every compaction and every recall into a permission error instead of a clean fallback."""
    env = os.environ if env is None else env
    for candidate in (explicit, env.get("ULTRACOMPRESS_BIN", ""), *BINARY_CANDIDATES):
        if candidate:
            path = Path(candidate).expanduser()
            if path.is_file() and os.access(path, os.X_OK):
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


def canonical_tool_name(name: Any) -> str:
    """The name a tool call carries into the brief (aliases mapped, otherwise unchanged)."""
    text = str(name or "").strip()
    return TOOL_NAME_ALIASES.get(text.lower(), text)


def tool_arguments(tc: Dict[str, Any]) -> Dict[str, Any]:
    """A tool call's arguments as a dict (harnesses store them as a JSON string)."""
    fn = tc.get("function") if isinstance(tc.get("function"), dict) else tc
    args = fn.get("arguments")
    if isinstance(args, str):
        try:
            args = json.loads(args)
        except ValueError:
            return {}
    return args if isinstance(args, dict) else {}


def _first_text(args: Dict[str, Any], keys: Iterable[str]) -> str:
    for key in keys:
        value = args.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return ""


def retained_tool_call(tc: Dict[str, Any]) -> Optional[Dict[str, Any]]:
    """The toolCall block the brief keeps for one call, or None when it carries no file/commit fact.

    A brief that forgets which files were written or edited, and which commits were made, leaves
    the next window without that record: the CLI builds its sticky Files And Changes and Commits
    sections from toolCall arguments, so those calls survive. Arguments are reduced to the path
    (or to the commit command), and read-only calls and every tool result stay out of the brief."""
    raw = (tc.get("function") or {}).get("name") if isinstance(tc.get("function"), dict) else None
    name = canonical_tool_name(raw or tc.get("name"))
    lower = name.lower()
    if lower in WRITE_TOOL_NAMES or lower in EDIT_TOOL_NAMES:
        path = _first_text(tool_arguments(tc),
                           ("path", "file_path", "filePath", "filename", "file", "target"))
        if not path:
            return None
        return _tool_call(tc, "write" if lower in WRITE_TOOL_NAMES else "edit",
                          {"path": path[:TOOL_ARGUMENT_CHARS]})
    if lower in SHELL_TOOL_NAMES:
        command = _first_text(tool_arguments(tc), ("command", "cmd", "script", "input"))
        if COMMIT_MARKER not in command.lower():
            return None
        return _tool_call(tc, "bash", {"command": command[:TOOL_ARGUMENT_CHARS]})
    return None


def _tool_call(tc: Dict[str, Any], name: str, arguments: Dict[str, Any]) -> Dict[str, Any]:
    """An OpenAI-shaped tool call carrying only the argument the brief needs."""
    return {"id": tc.get("id"), "type": "function",
            "function": {"name": name, "arguments": arguments}}


def turn_chars(turns: Iterable[Dict[str, Any]]) -> int:
    """Window size in characters, from message text and tool-call arguments.

    The status line needs an estimate, not a second serialisation of the whole window."""
    total = 0
    for t in turns:
        total += len(text_of(t.get("content")))
        for tc in t.get("tool_calls") or []:
            args = (tc.get("function") or {}).get("arguments") if isinstance(tc.get("function"), dict) else None
            if isinstance(args, dict):
                total += sum(len(str(v)) for v in args.values())
            else:
                total += len(args or "")
    return total


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
    default tool results and read-only tool calls are left out of the brief (the raw archive keeps
    them for recall). The calls that write or edit a file, and shell calls that commit, stay as
    toolCall blocks with a reduced argument: without them the brief cannot record which files
    changed or which commits landed. tool_result_chars > 0 keeps that many characters of each tool
    result instead. Earlier compaction handoffs are skipped: they reach the CLI as previousSummary."""
    view: List[Dict[str, Any]] = []
    for t in turns:
        if is_carrier(t):
            continue
        role = t.get("role")
        if role == "tool":
            if tool_result_chars > 0:
                view.append({**t, "content": unwrap_tool_text(text_of(t.get("content")))[:tool_result_chars]})
        elif role == "assistant" and t.get("tool_calls"):
            if tool_result_chars > 0:
                view.append(t)
                continue
            calls = [c for c in (retained_tool_call(tc) for tc in t["tool_calls"]) if c]
            text = text_of(t.get("content"))
            if text.strip() or calls:
                kept: Dict[str, Any] = {"role": "assistant", "content": text}
                if calls:
                    kept["tool_calls"] = calls
                view.append(kept)
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


EMPTY_WINDOW_LINE = ("Condensed turns held no briefable text of their own; the earlier brief stands "
                     "and the raw archive keeps the rest searchable.")
BRIEF_INTRO = "Deterministic brief of the condensed turns"


def _without_intro(previous_brief: str) -> str:
    """A previous brief's body: compose_brief adds the heading and intro again."""
    body = previous_brief.strip()
    if body.startswith(BRIEF_INTRO):  # compose_brief adds the intro again
        body = body.split("\n\n", 1)[1].strip() if "\n\n" in body else ""
    return body


def empty_window_brief(previous_brief: str = "") -> str:
    """Brief for a window the CLI found nothing to say about: one local line, no model call."""
    previous = _without_intro(previous_brief)
    return f"{previous}\n\n{EMPTY_WINDOW_LINE}" if previous else EMPTY_WINDOW_LINE


def tool_only_brief(turns: Iterable[Dict[str, Any]], previous_brief: str = "") -> str:
    """Brief for a window with tool steps only: the previous brief plus which tools ran, by count."""
    counts: Dict[str, int] = {}
    for t in turns:
        for tc in t.get("tool_calls") or []:
            name = (tc.get("function") or {}).get("name") or tc.get("name") or "tool"
            counts[name] = counts.get(name, 0) + 1
    used = ", ".join(f"{n} ×{c}" if c > 1 else n for n, c in sorted(counts.items(), key=lambda kv: -kv[1]))
    line = f"Tool steps condensed with nothing said: {sum(counts.values())} call(s) ({used or 'none'})."
    previous = _without_intro(previous_brief)
    return f"{previous}\n\n{line}" if previous else line


def compose_brief(brief: str) -> str:
    return (
        BRIEF_HEADING + "\n" +
        BRIEF_INTRO + " (built locally, no model call). Anything not "
        f"shown here is still searchable with the {RECALL_TOOL} tool.\n\n" + brief
    )
