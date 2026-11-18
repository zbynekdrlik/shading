"""scripts/discord_thread.py (shading#24): only the feedback thread, never another channel."""

import importlib.util
from pathlib import Path

import pytest

SPEC = importlib.util.spec_from_file_location(
    "discord_thread", Path(__file__).resolve().parents[2] / "scripts" / "discord_thread.py"
)
dt = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(dt)


def test_only_the_feedback_thread_is_allowed():
    assert dt.check_channel(dt.THREAD_ID) == dt.THREAD_ID
    with pytest.raises(dt.DiscordError):
        dt.check_channel("1062019161523507311")  # the parent channel, not the thread


def test_main_refuses_another_channel_before_any_request(monkeypatch, capsys):
    monkeypatch.setenv("DISCORD_BOT_TOKEN", "x")
    called = []
    monkeypatch.setattr(dt, "request", lambda *a, **k: called.append(a))
    assert dt.main(["--channel", "123", "read"]) == 1
    assert called == []
    assert "refused" in capsys.readouterr().err


def test_main_needs_the_token(monkeypatch, capsys):
    monkeypatch.delenv("DISCORD_BOT_TOKEN", raising=False)
    assert dt.main(["read"]) == 1
    assert "DISCORD_BOT_TOKEN" in capsys.readouterr().err


def test_post_refuses_empty_and_too_long(monkeypatch):
    monkeypatch.setattr(dt, "request", lambda *a, **k: pytest.fail("no request expected"))
    with pytest.raises(dt.DiscordError):
        dt.cmd_post("t", dt.THREAD_ID, "  ", None)
    with pytest.raises(dt.DiscordError):
        dt.cmd_post("t", dt.THREAD_ID, "x" * 2001, None)


def test_post_reply_shape_and_no_mentions(monkeypatch, capsys):
    sent = {}

    def fake(method, path, token, body=None):
        sent.update(method=method, path=path, body=body)
        return {"id": "42"}

    monkeypatch.setattr(dt, "request", fake)
    assert dt.cmd_post("t", dt.THREAD_ID, "ahoj", "7") == 0
    assert sent["path"] == f"/channels/{dt.THREAD_ID}/messages"
    assert sent["body"]["message_reference"]["message_id"] == "7"
    assert sent["body"]["allowed_mentions"] == {"parse": []}
    assert "posted 42" in capsys.readouterr().out


def test_read_lists_oldest_first_and_remembers_the_newest(monkeypatch, tmp_path, capsys):
    monkeypatch.setattr(dt, "STATE", tmp_path / "last")
    msgs = [
        {"id": "20", "content": "druha", "author": {"username": "b"}, "attachments": [{"filename": "a.png", "url": "u"}]},
        {"id": "10", "content": "prva", "author": {"username": "a"}},
    ]
    monkeypatch.setattr(dt, "request", lambda *a, **k: msgs)
    assert dt.cmd_read("t", dt.THREAD_ID, False) == 0
    out = capsys.readouterr().out
    assert out.index("prva") < out.index("druha")
    assert "attachment: a.png" in out
    assert dt.read_last_id() == "20"


def test_react_puts_the_worker_emoji_on_the_thread_only(monkeypatch):
    seen = []
    monkeypatch.setattr(dt, "request", lambda method, path, token, body=None: seen.append((method, path)))
    assert dt.cmd_react("t", dt.THREAD_ID, "55", "\U0001F477") == 0
    assert seen == [("PUT", f"/channels/{dt.THREAD_ID}/messages/55/reactions/%F0%9F%91%B7/@me")]
    with pytest.raises(dt.DiscordError):
        dt.cmd_react("t", "999", "55", "x")
    with pytest.raises(dt.DiscordError):
        dt.cmd_react("t", dt.THREAD_ID, "../x", "x")
