#!/usr/bin/env python3
"""TUIをtmuxの中で動かし、キーとマウスを送って、画面とクリップボードを読む（folioの動作確認用）。

座標はすべて「桁 行」の順で、1始まり（端末のマウスの信号と同じ）。

例:
  scripts/tuidrive.py start --size 140x24 -- target/debug/folio browse src
  scripts/tuidrive.py click $(scripts/tuidrive.py find select.rs) --right
  scripts/tuidrive.py screen --reversed
  scripts/tuidrive.py key C-c
  scripts/tuidrive.py clipboard
  scripts/tuidrive.py stop

罠を避けるための既定:
- 利用者の`tmux.conf`は読まない（`mouse on`などで結果が変わるため）
- `set-clipboard on`にして、アプリがOSC 52で送った文字を`clipboard`で読めるようにする
- アプリが終わってもペインを残す（`alive`で終了を確かめられる）
- ホイールは押す信号だけを送る（離す信号も送ると2回と数えられる）
"""

import argparse
import re
import shlex
import subprocess
import sys
import time
import unicodedata

SESSION = "t"
BUTTONS = {"left": 0, "middle": 1, "right": 2}
DRAG = 32
WHEEL = {"up": 64, "down": 65}
SGR_RE = re.compile(r"(\x1b\[[0-9;]*m)")


# ---- 端末に送る列・画面の読み取り（tmuxに触らない） ----


def sgr_mouse(code, col, row, release=False):
    """SGR形式のマウスの信号（押す`M`、離す`m`）。"""
    return f"\x1b[<{code};{col};{row}{'m' if release else 'M'}"


def click_seqs(col, row, button="left", double=False):
    code = BUTTONS[button]
    one = [sgr_mouse(code, col, row), sgr_mouse(code, col, row, release=True)]
    return one * (2 if double else 1)


def drag_seqs(col1, row1, col2, row2, steps=3):
    """押して、`steps`回に分けて動かし、離す。"""
    seqs = [sgr_mouse(BUTTONS["left"], col1, row1)]
    for i in range(1, steps + 1):
        col = col1 + (col2 - col1) * i // steps
        row = row1 + (row2 - row1) * i // steps
        seqs.append(sgr_mouse(BUTTONS["left"] + DRAG, col, row))
    seqs.append(sgr_mouse(BUTTONS["left"], col2, row2, release=True))
    return seqs


def wheel_seqs(direction, col, row, count=1):
    return [sgr_mouse(WHEEL[direction], col, row)] * count


def char_width(ch):
    """端末での表示幅。全角（W/F）は2、結合文字は0、それ以外は1。"""
    if unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in "WF" else 1


def display_width(text):
    return sum(char_width(c) for c in text)


def find_text(screen, text):
    """画面の文字列で`text`が最初に出る位置（桁, 行）。無ければNone。"""
    for row, line in enumerate(screen.split("\n"), start=1):
        i = line.find(text)
        if i >= 0:
            return display_width(line[:i]) + 1, row
    return None


def _apply_sgr(params, reverse):
    """SGRの引数を読んで、反転の状態を返す。拡張色（38/48/58）の引数は読み飛ばす。"""
    nums = [int(p) if p else 0 for p in params.split(";")] if params else [0]
    i = 0
    while i < len(nums):
        n = nums[i]
        if n in (38, 48, 58) and i + 1 < len(nums):
            i += 3 if nums[i + 1] == 5 else 5 if nums[i + 1] == 2 else 1
            continue
        if n == 0 or n == 27:
            reverse = False
        elif n == 7:
            reverse = True
        i += 1
    return reverse


def reversed_runs(ansi_screen):
    """`capture-pane -e`の出力から、反転している範囲を(行, 始めの桁, 終わりの桁, 文字)で返す。"""
    runs = []
    for row, line in enumerate(ansi_screen.split("\n"), start=1):
        reverse = False
        col = 1
        current = None  # [始めの桁, 終わりの桁, 文字]
        for part in SGR_RE.split(line):
            if SGR_RE.fullmatch(part):
                reverse = _apply_sgr(part[2:-1], reverse)
                continue
            for ch in part:
                width = char_width(ch)
                if reverse and width > 0:
                    if current is None:
                        current = [col, col + width - 1, ch]
                    else:
                        current[1] = col + width - 1
                        current[2] += ch
                elif not reverse and current is not None:
                    runs.append((row, *current))
                    current = None
                col += width
            if not reverse and current is not None:
                runs.append((row, *current))
                current = None
        if current is not None:
            runs.append((row, *current))
    return runs


# ---- tmux ----


def tmux(socket, *args, capture=False, check=True):
    result = subprocess.run(
        ["tmux", "-L", socket, "-f", "/dev/null", *args],
        capture_output=capture,
        text=True,
        check=check,
    )
    return result.stdout if capture else result.returncode


def send_seqs(socket, seqs, delay):
    for seq in seqs:
        tmux(socket, "send-keys", "-t", SESSION, "-l", seq)
        time.sleep(delay)


def capture(socket, escapes=False):
    args = ["capture-pane", "-t", SESSION, "-p"]
    if escapes:
        args.append("-e")
    return tmux(socket, *args, capture=True)


def cmd_start(a):
    width, height = (int(v) for v in a.size.split("x"))
    tmux(a.socket, "kill-server", check=False, capture=True)
    command = shlex.join(a.command)
    tmux(
        a.socket,
        "start-server", ";",
        "set-option", "-g", "remain-on-exit", "on", ";",
        "set-option", "-g", "set-clipboard", "on", ";",
        "new-session", "-d", "-s", SESSION, "-x", str(width), "-y", str(height), command,
    )
    time.sleep(a.wait)


def cmd_find(a):
    pos = find_text(capture(a.socket), a.text)
    if pos is None:
        print(f"見つかりません: {a.text}", file=sys.stderr)
        return 1
    print(*pos)
    return 0


def cmd_screen(a):
    if a.reversed:
        for row, start, end, text in reversed_runs(capture(a.socket, escapes=True)):
            print(f"{row}行 {start}-{end}桁: {text}")
    else:
        print(capture(a.socket), end="")
    return 0


def cmd_clipboard(a):
    result = subprocess.run(
        ["tmux", "-L", a.socket, "-f", "/dev/null", "show-buffer"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return 1
    print(result.stdout, end="")
    return 0


def cmd_alive(a):
    dead = tmux(a.socket, "display-message", "-p", "-t", SESSION, "#{pane_dead}", capture=True)
    alive = dead.strip() != "1"
    print("動作中" if alive else "終了")
    return 0 if alive else 1


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--socket", default="tuidrive", help="tmuxのソケット名（試験ごとに分けると並行して動かせる）")
    p.add_argument("--delay", type=float, default=0.15, help="送るたびに待つ秒数")
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("start", help="tmuxを立ててコマンドを起動する")
    s.add_argument("--size", default="140x24", help="幅x高さ（一覧の項目数より高くする）")
    s.add_argument("--wait", type=float, default=1.2, help="起動後に待つ秒数")
    s.add_argument("command", nargs="+")

    c = sub.add_parser("click", help="クリック")
    c.add_argument("col", type=int)
    c.add_argument("row", type=int)
    c.add_argument("--right", action="store_true")
    c.add_argument("--double", action="store_true")

    d = sub.add_parser("drag", help="左ボタンでドラッグ")
    for name in ("col1", "row1", "col2", "row2"):
        d.add_argument(name, type=int)

    w = sub.add_parser("wheel", help="ホイール")
    w.add_argument("direction", choices=["up", "down"])
    w.add_argument("col", type=int)
    w.add_argument("row", type=int)
    w.add_argument("--count", type=int, default=1)

    k = sub.add_parser("key", help="tmuxのキー名で送る（j, Enter, Escape, C-cなど）")
    k.add_argument("keys", nargs="+")

    t = sub.add_parser("type", help="文字列をそのまま送る")
    t.add_argument("text")

    f = sub.add_parser("find", help="文字の位置を「桁 行」で出す")
    f.add_argument("text")

    sc = sub.add_parser("screen", help="画面の文字を出す")
    sc.add_argument("--reversed", action="store_true", help="反転している範囲だけを出す")

    sub.add_parser("clipboard", help="アプリがクリップボードへ送った文字（無ければ終了コード1）")
    sub.add_parser("alive", help="アプリが動いているか（終わっていれば終了コード1）")
    sub.add_parser("stop", help="tmuxを片付ける")

    a = p.parse_args(argv)
    if a.cmd == "start":
        cmd_start(a)
    elif a.cmd == "click":
        send_seqs(a.socket, click_seqs(a.col, a.row, "right" if a.right else "left", a.double), a.delay)
    elif a.cmd == "drag":
        send_seqs(a.socket, drag_seqs(a.col1, a.row1, a.col2, a.row2), a.delay)
    elif a.cmd == "wheel":
        send_seqs(a.socket, wheel_seqs(a.direction, a.col, a.row, a.count), a.delay)
    elif a.cmd == "key":
        for key in a.keys:
            tmux(a.socket, "send-keys", "-t", SESSION, key)
            time.sleep(a.delay)
    elif a.cmd == "type":
        send_seqs(a.socket, [a.text], a.delay)
    elif a.cmd == "find":
        return cmd_find(a)
    elif a.cmd == "screen":
        return cmd_screen(a)
    elif a.cmd == "clipboard":
        return cmd_clipboard(a)
    elif a.cmd == "alive":
        return cmd_alive(a)
    elif a.cmd == "stop":
        tmux(a.socket, "kill-server", check=False, capture=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
