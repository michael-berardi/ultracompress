"""UltraCompress context engine for Hermes Agent: a ContextEngine registered via register_context_engine.

Hermes keeps its own compaction pipeline (when to compact, which turns stay, where the handoff
goes, how the session database is rewritten). This engine replaces only the summary step: instead
of asking a model to summarize the condensed turns, it runs `ultracompress compact` locally and
inserts the deterministic brief, followed by verbatim excerpts of the agent's replies and Hermes'
own deterministic sections (anchor index, verbatim user messages, recovery footer). Condensed turns
are archived as a Pi-format session per Hermes session, and the `ultracompress_recall` tool
searches that archive.

Install: copy or symlink this folder to $HERMES_HOME/plugins/ultracompress, then
`hermes config set context.engine ultracompress`. See README.md next to this folder.

Settings: optional settings.json next to this file, for example
  {"mode": "deterministic", "threshold": 0.30}
  mode       deterministic (default) | builtin (behave exactly like Hermes' ContextCompressor)
  threshold  compaction trigger as a fraction of the context window. Hermes' own compressor
             raises anything under a 512k window to 75%; this engine honours the value as given,
             because a deterministic compaction costs milliseconds, not a model call.
  binary, archive_dir, timeout_seconds, tool_result_chars, reply_chars, replies_budget_chars,
  carry_users_chars: see DEFAULT_SETTINGS.
Any UltraCompress failure is logged with its cause, and Hermes' built-in summary runs for that
compaction instead.
"""
from __future__ import annotations

import json
import logging
import os
import re
import subprocess
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

from agent.context_compressor import ContextCompressor

from .convert import (RECALL_TOOL, agent_replies_section, carry_forward_users, compose_brief,
                      conversation_view, find_binary, section, tool_only_brief, BRIEF_HEADING, to_pi_entries)

__version__ = "0.1.0"
logger = logging.getLogger("plugins.ultracompress")

PLUGIN_DIR = Path(__file__).resolve().parent
SETTINGS_FILE = PLUGIN_DIR / "settings.json"
DEFAULT_SETTINGS: Dict[str, Any] = {
    "mode": "deterministic",
    "threshold": 0.30,
    "binary": "",                 # "" = $ULTRACOMPRESS_BIN, then the standard install locations
    "archive_dir": "",            # "" = $HERMES_HOME/ultracompress
    "timeout_seconds": 30,
    "tool_result_chars": 0,       # 0 = tool output stays out of the brief (it is archived for recall)
    "reply_chars": 1200,
    "replies_budget_chars": 12000,
    "carry_users_chars": 4000,
}


def load_settings() -> Dict[str, Any]:
    settings = dict(DEFAULT_SETTINGS)
    try:
        settings.update(json.loads(SETTINGS_FILE.read_text()))
    except FileNotFoundError:
        pass
    except (OSError, ValueError) as exc:
        logger.warning("UltraCompress: ignoring unreadable %s (%s); using defaults", SETTINGS_FILE, exc)
    return settings


def _hermes_home() -> Path:
    try:
        from hermes_constants import get_hermes_home
        return Path(get_hermes_home())
    except Exception:
        return Path(os.environ.get("HERMES_HOME") or Path.home() / ".hermes")


def _is_carrier(message: Dict[str, Any]) -> bool:
    try:
        from agent.context_compressor import is_compaction_summary_message
        return bool(is_compaction_summary_message(message))
    except Exception:
        content = message.get("content")
        return isinstance(content, str) and content.lstrip().startswith("[CONTEXT COMPACTION")


def _read_config() -> Dict[str, Any]:
    try:
        from hermes_cli.config import load_config_readonly
        return load_config_readonly() or {}
    except Exception as exc:
        logger.warning("UltraCompress: could not read Hermes config (%s); using defaults", exc)
        return {}


class UltraCompressEngine(ContextCompressor):
    """Hermes' ContextCompressor with UltraCompress as the summary step."""

    @property
    def name(self) -> str:
        return "ultracompress"

    def __init__(self) -> None:
        cfg = _read_config()
        compression = cfg.get("compression", {}) or {}
        model = cfg.get("model", {}) or {}
        self.uc_settings = load_settings()
        super().__init__(
            model=str(model.get("default") or model.get("model") or ""),
            provider=str(model.get("provider") or ""),
            threshold_percent=float(self.uc_settings.get("threshold") or compression.get("threshold") or 0.30),
            protect_first_n=int(compression.get("protect_first_n", 3)),
            protect_last_n=int(compression.get("protect_last_n", 20)),
            summary_target_ratio=float(compression.get("target_ratio", 0.20)),
            quiet_mode=True,
            min_tail_user_messages=int(compression.get("min_tail_user_messages", 1)),
        )
        self.last_uc_report: Dict[str, Any] = {}

    # Deterministic compaction is cheap, so the configured threshold is used as given.
    def _effective_threshold_percent(self, context_length: int, threshold_percent: float) -> float:  # type: ignore[override]
        if self.uc_settings.get("mode") == "builtin":
            return ContextCompressor._effective_threshold_percent(context_length, threshold_percent)
        return threshold_percent

    def _generate_summary(self, turns_to_summarize: List[Dict[str, Any]], focus_topic: Optional[str] = None,
                          memory_context: str = "", bypass_cooldown: bool = False) -> Optional[str]:
        if self.uc_settings.get("mode") == "builtin":
            return super()._generate_summary(turns_to_summarize, focus_topic, memory_context, bypass_cooldown)
        started = time.monotonic()
        try:
            brief, report = self._ultracompress_brief(turns_to_summarize)
        except Exception as exc:
            logger.warning("UltraCompress failed (%s); using Hermes' built-in summary for this compaction", exc)
            self.last_uc_report = {"ok": False, "error": str(exc)}
            return super()._generate_summary(turns_to_summarize, focus_topic, memory_context, bypass_cooldown)
        self._archive(turns_to_summarize)
        from agent.context_compressor import _redact_compaction_text

        previous = self._strip_summary_prefix(self._previous_summary or "")
        replies = agent_replies_section(turns_to_summarize, previous,
                                        per_reply=int(self.uc_settings["reply_chars"]),
                                        total=int(self.uc_settings["replies_budget_chars"]))
        summary = _redact_compaction_text(compose_brief(brief) + replies)
        summary = self._augment_summary_lean(summary, turns_to_summarize)
        summary = carry_forward_users(summary, previous, int(self.uc_settings["carry_users_chars"]))
        self._previous_summary = summary
        self._clear_compression_failure_cooldown()
        self._last_summary_error = None
        raw_chars = sum(len(json.dumps(t, ensure_ascii=False, default=str)) for t in turns_to_summarize)
        report.update(ok=True, ms=round((time.monotonic() - started) * 1000),
                      raw_tokens_est=raw_chars // 3, summary_tokens_est=len(summary) // 3)
        self.last_uc_report = report
        logger.info("UltraCompress · %s turns (~%s tokens) → summary ~%s tokens · %s ms",
                    len(turns_to_summarize), report["raw_tokens_est"], report["summary_tokens_est"], report["ms"])
        return self._with_summary_prefix(summary)

    def _binary(self) -> str:
        binary = find_binary(str(self.uc_settings.get("binary") or ""))
        if not binary:
            raise RuntimeError("ultracompress binary not found (settings.binary, $ULTRACOMPRESS_BIN, "
                               "~/.local/bin, ~/.ultraterm/bin, /opt/homebrew/bin, /usr/local/bin)")
        return binary

    def _ultracompress_brief(self, turns: List[Dict[str, Any]]):
        view = conversation_view(turns, int(self.uc_settings.get("tool_result_chars") or 0), _is_carrier)
        entries = to_pi_entries(view)
        previous_brief = ""
        if self._previous_summary:
            previous = self._strip_summary_prefix(self._previous_summary)
            previous_brief = section(previous, BRIEF_HEADING) or previous
        if not entries:
            # A stretch of tool calls with nothing said (a long tool loop): there is no text for the
            # CLI, and a model summary here costs a minute. Keep the previous brief and name the tools;
            # the raw archive keeps the results for recall.
            return tool_only_brief(turns, previous_brief), {"toolOnly": True}
        payload: Dict[str, Any] = {"entries": entries, "keepUserTurns": 0, "smartKeepTail": False}
        if previous_brief:
            payload["previousSummary"] = previous_brief
        proc = subprocess.run([self._binary(), "compact", "--policy", "vcc", "--keep", "0"],
                              input=json.dumps(payload), capture_output=True, text=True,
                              timeout=float(self.uc_settings.get("timeout_seconds") or 30))
        if proc.returncode != 0:
            raise RuntimeError(f"ultracompress exited {proc.returncode}: {proc.stderr.strip()[:300]}")
        out = json.loads(proc.stdout)
        brief = (out.get("summary") or "").strip()
        if not brief:
            raise RuntimeError("ultracompress returned an empty summary")
        return brief, dict(out.get("stats") or {})

    # ---- raw archive + recall -----------------------------------------------------------------
    def _archive_path(self) -> Path:
        base = Path(self.uc_settings.get("archive_dir") or _hermes_home() / "ultracompress").expanduser()
        sid = re.sub(r"[^A-Za-z0-9_.-]", "_", getattr(self, "_session_id", "") or "unbound")
        return base / f"{sid}.jsonl"

    def on_session_start(self, session_id: str, **kwargs) -> None:
        """Hermes may rotate to a new session id at a compaction boundary; carry the archive over so
        recall still reaches turns condensed under the old id."""
        super().on_session_start(session_id, **kwargs)
        old_id = kwargs.get("old_session_id")
        if not old_id or old_id == session_id:
            return
        try:
            new_path = self._archive_path()
            old_path = new_path.with_name(re.sub(r"[^A-Za-z0-9_.-]", "_", str(old_id)) + ".jsonl")
            if old_path.exists() and not new_path.exists():
                new_path.write_bytes(old_path.read_bytes())
                os.chmod(new_path, 0o600)
        except Exception as exc:
            logger.warning("UltraCompress: could not carry the recall archive across session rotation (%s)", exc)

    def _archive(self, turns: List[Dict[str, Any]]) -> None:
        """Append condensed turns (Pi format, private file) so recall can find them. Never fatal."""
        try:
            path = self._archive_path()
            path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            fresh = not path.exists()
            stamp = f"c{int(time.time() * 1000)}-"
            with path.open("a", encoding="utf-8") as fh:
                if fresh:
                    fh.write(json.dumps({"type": "session", "version": 3, "id": path.stem}) + "\n")
                for entry in to_pi_entries([t for t in turns if not _is_carrier(t)], id_prefix=stamp):
                    fh.write(json.dumps(entry, ensure_ascii=False) + "\n")
            os.chmod(path, 0o600)
        except Exception as exc:
            logger.warning("UltraCompress: could not archive condensed turns (%s); recall may miss them", exc)

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        return [{
            "name": RECALL_TOOL,
            "description": ("Search the raw text of turns that context compaction removed from this conversation "
                            "(ranked keywords, or a regular expression). Use it when the compaction summary mentions "
                            "something you need verbatim: the user's exact words, an id, a number, a tool result."),
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Keywords, or a regular expression when regex is true."},
                    "regex": {"type": "boolean", "description": "Treat query as a regular expression."},
                    "role": {"type": "string", "enum": ["user", "assistant", "toolResult"],
                             "description": "Only search one role."},
                },
                "required": ["query"],
            },
        }]

    def handle_tool_call(self, name: str, args: Dict[str, Any], **kwargs) -> str:
        if name != RECALL_TOOL:
            return json.dumps({"error": f"unknown tool {name}"})
        query = str((args or {}).get("query") or "").strip()
        if not query:
            return json.dumps({"error": "query is required"})
        path = self._archive_path()
        if not path.exists():
            return json.dumps({"result": "Nothing has been compacted in this conversation yet; it is all still in context."})
        try:
            cmd = [self._binary(), "recall", "--session", str(path), "--scope", "all",
                   "--query", query, "--max-output-bytes", "12000"]
        except RuntimeError as exc:
            return json.dumps({"error": str(exc)})
        if args.get("regex"):
            cmd.append("--regex")
        if args.get("role"):
            cmd += ["--role", str(args["role"])]
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        except subprocess.TimeoutExpired:
            return json.dumps({"error": "recall timed out after 30 s"})
        if proc.returncode != 0:
            return json.dumps({"error": f"recall failed ({proc.returncode}): {proc.stderr.strip()[:300]}"})
        return proc.stdout.strip() or json.dumps({"result": "no matches"})

    def get_status(self) -> Dict[str, Any]:
        status = super().get_status()
        status["ultracompress"] = {"version": __version__, "mode": self.uc_settings.get("mode"),
                                   "last": self.last_uc_report}
        return status


def register(ctx) -> None:
    ctx.register_context_engine(UltraCompressEngine())
