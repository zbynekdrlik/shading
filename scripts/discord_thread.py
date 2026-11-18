#!/usr/bin/env python3
"""Read and reply in the ONE Discord thread where the operators give panel feedback (shading#24).

The thread is 'Grading NEW' (1561136322570231898, a public thread in channel 1062019161523507311,
guild 790859391330942986). The bot is 'shading' (airuleset#1368); its token comes ONLY from the
environment (`DISCORD_BOT_TOKEN`), injected by:

    python3 ~/devel/airuleset/airuleset.py secret exec --file ~/.secrets/discord-shading-bot \
        --env DISCORD_BOT_TOKEN -- python3 scripts/discord_thread.py read

Commands:
    read [--all] [--mentions-only]
                            messages newer than the last one seen (or the last 100 with --all),
                            oldest first, with attachment URLs; remembers the newest id.
                            --mentions-only prints only messages that @mention the bot (owner
                            18.11.2026: work starts ONLY when someone tags the bot; the rest is
                            the team's own discussion and is not processed)
    context                 the whole discussion since the bot's own last message (up to 100), so a
                            tag is answered with everything the team wrote before it in view
    post TEXT [--reply ID]  post TEXT to the thread, optionally as a reply to message ID
    react ID [EMOJI]        add a reaction (default 👷 = "taken, working on it") to message ID

Any other channel id is refused: the thread id is a constant and `--channel` must equal it.
Stdlib only. Fails loudly (non-zero exit with the HTTP status and Discord's answer).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

THREAD_ID = "1561136322570231898"
BOT_ID = "1572692720793813114"  # the 'shading' bot (airuleset#1368)
API = "https://discord.com/api/v10"
USER_AGENT = "DiscordBot (shading, 1)"
STATE = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local" / "state")) / "shading" / "discord-last-id"
MAX_LEN = 2000  # Discord's message limit


class DiscordError(RuntimeError):
    pass


def check_channel(channel: str) -> str:
    """The only channel this tool may touch is the feedback thread."""
    if channel != THREAD_ID:
        raise DiscordError(f"refused: channel {channel!r} is not the feedback thread {THREAD_ID}")
    return channel


def request(method: str, path: str, token: str, body: dict | None = None) -> object:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(API + path, data=data, method=method)
    req.add_header("Authorization", f"Bot {token}")
    req.add_header("User-Agent", USER_AGENT)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            raw = resp.read()
    except urllib.error.HTTPError as e:
        raise DiscordError(f"{method} {path} -> HTTP {e.code}: {e.read()[:300].decode(errors='replace')}") from e
    except urllib.error.URLError as e:
        raise DiscordError(f"{method} {path} failed: {e.reason}") from e
    return json.loads(raw) if raw else None


def read_last_id() -> str | None:
    try:
        v = STATE.read_text().strip()
    except FileNotFoundError:
        return None
    return v or None


def write_last_id(msg_id: str) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(msg_id + "\n")
    tmp.replace(STATE)


def format_message(m: dict) -> str:
    author = (m.get("author") or {}).get("global_name") or (m.get("author") or {}).get("username", "?")
    ref = (m.get("message_reference") or {}).get("message_id")
    head = f"[{m['id']}] {m.get('timestamp', '')[:19]} {author}" + (f" (reply to {ref})" if ref else "")
    lines = [head, m.get("content", "")]
    for a in m.get("attachments") or []:
        lines.append(f"  attachment: {a.get('filename')} {a.get('content_type', '')} {a.get('url')}")
    for e in m.get("embeds") or []:
        if e.get("url") or e.get("title"):
            lines.append(f"  embed: {e.get('title', '')} {e.get('url', '')}")
    return "\n".join(lines)


def mentions_bot(m: dict) -> bool:
    """A message addressed to the bot: an @mention of it, never the bot's own message."""
    if (m.get("author") or {}).get("id") == BOT_ID:
        return False
    return any(u.get("id") == BOT_ID for u in m.get("mentions") or []) or f"<@{BOT_ID}>" in (m.get("content") or "")


def cmd_read(token: str, channel: str, all_: bool, mentions_only: bool = False) -> int:
    check_channel(channel)
    after = None if all_ else read_last_id()
    path = f"/channels/{channel}/messages?limit=100" + (f"&after={after}" if after else "")
    msgs = request("GET", path, token)
    if not isinstance(msgs, list):
        raise DiscordError(f"unexpected answer: {str(msgs)[:200]}")
    msgs.sort(key=lambda m: int(m["id"]))
    shown = [m for m in msgs if mentions_bot(m)] if mentions_only else msgs
    for m in shown:
        print(format_message(m))
        print("---")
    if msgs:
        write_last_id(msgs[-1]["id"])
    print(f"{len(msgs)} message(s), {len(shown)} shown; last seen id {read_last_id()}", file=sys.stderr)
    return 0


def since_last_bot(msgs: list[dict]) -> list[dict]:
    """The messages after the bot's own last one (oldest first); all of them when it never wrote."""
    msgs = sorted(msgs, key=lambda m: int(m["id"]))
    last = max((i for i, m in enumerate(msgs) if (m.get("author") or {}).get("id") == BOT_ID), default=-1)
    return msgs[last + 1:]


def cmd_context(token: str, channel: str) -> int:
    check_channel(channel)
    msgs = request("GET", f"/channels/{channel}/messages?limit=100", token)
    if not isinstance(msgs, list):
        raise DiscordError(f"unexpected answer: {str(msgs)[:200]}")
    ctx = since_last_bot(msgs)
    for m in ctx:
        print(format_message(m))
        print("---")
    print(f"{len(ctx)} message(s) since the bot's last message", file=sys.stderr)
    return 0


def cmd_post(token: str, channel: str, text: str, reply: str | None) -> int:
    check_channel(channel)
    if not text.strip():
        raise DiscordError("refused: empty message")
    if len(text) > MAX_LEN:
        raise DiscordError(f"refused: {len(text)} characters, Discord allows {MAX_LEN}")
    body: dict = {"content": text, "allowed_mentions": {"parse": []}}
    if reply:
        body["message_reference"] = {"message_id": reply, "fail_if_not_exists": False}
    m = request("POST", f"/channels/{channel}/messages", token, body)
    print(f"posted {m['id']}")
    return 0


def cmd_react(token: str, channel: str, msg_id: str, emoji: str) -> int:
    check_channel(channel)
    if not msg_id.isdigit():
        raise DiscordError(f"refused: {msg_id!r} is not a message id")
    request("PUT", f"/channels/{channel}/messages/{msg_id}/reactions/{urllib.parse.quote(emoji)}/@me", token)
    print(f"reacted {emoji} on {msg_id}")
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--channel", default=THREAD_ID, help="must be the feedback thread id")
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("read")
    r.add_argument("--all", action="store_true", help="the last 100 messages, not only new ones")
    r.add_argument("--mentions-only", action="store_true", help="only messages that @mention the bot")
    w = sub.add_parser("post")
    w.add_argument("text")
    w.add_argument("--reply", help="message id to reply to")
    sub.add_parser("context")
    a = sub.add_parser("react")
    a.add_argument("message_id")
    a.add_argument("emoji", nargs="?", default="\U0001F477")
    args = p.parse_args(argv)
    try:
        check_channel(args.channel)
        token = os.environ.get("DISCORD_BOT_TOKEN", "").strip()
        if not token:
            raise DiscordError("DISCORD_BOT_TOKEN is not set (run through `airuleset.py secret exec`)")
        if args.cmd == "read":
            return cmd_read(token, args.channel, args.all, args.mentions_only)
        if args.cmd == "context":
            return cmd_context(token, args.channel)
        if args.cmd == "react":
            return cmd_react(token, args.channel, args.message_id, args.emoji)
        return cmd_post(token, args.channel, args.text, args.reply)
    except DiscordError as e:
        print(f"discord_thread: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
