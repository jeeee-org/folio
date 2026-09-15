"""scripts/tuidrive.pyのテスト。`python3 -m unittest discover -s scripts`で動かす。

tmuxが無い環境では、実際にtmuxを立てる試験だけを飛ばす。
"""

import os
import shutil
import subprocess
import sys
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import tuidrive as td  # noqa: E402

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "tuidrive.py")


class Sequences(unittest.TestCase):
    def test_click_sends_press_and_release(self):
        self.assertEqual(td.click_seqs(5, 3), ["\x1b[<0;5;3M", "\x1b[<0;5;3m"])
        self.assertEqual(td.click_seqs(5, 3, "right")[0], "\x1b[<2;5;3M")
        self.assertEqual(len(td.click_seqs(5, 3, double=True)), 4)

    def test_drag_moves_with_button_held(self):
        seqs = td.drag_seqs(10, 2, 40, 5, steps=3)
        self.assertEqual(seqs[0], "\x1b[<0;10;2M")
        self.assertEqual(seqs[1:4], ["\x1b[<32;20;3M", "\x1b[<32;30;4M", "\x1b[<32;40;5M"])
        self.assertEqual(seqs[-1], "\x1b[<0;40;5m")

    def test_wheel_sends_press_only(self):
        # 離す信号も送ると、アプリが2回と数えてしまう
        self.assertEqual(td.wheel_seqs("down", 7, 8, count=2), ["\x1b[<65;7;8M"] * 2)
        self.assertEqual(td.wheel_seqs("up", 7, 8), ["\x1b[<64;7;8M"])


class Screen(unittest.TestCase):
    def test_find_counts_wide_chars_as_two_columns(self):
        screen = "abc\n 見出し: select.rs\n"
        self.assertEqual(td.find_text(screen, "select"), (10, 2))
        self.assertEqual(td.find_text(screen, "abc"), (1, 1))
        self.assertIsNone(td.find_text(screen, "zzz"))

    def test_reversed_runs_track_sgr(self):
        line = "ab\x1b[7mcd\x1b[0me\x1b[7mあ\x1b[27mf"
        self.assertEqual(
            td.reversed_runs(line),
            [(1, 3, 4, "cd"), (1, 6, 7, "あ")],
        )

    def test_reversed_runs_skip_extended_color_arguments(self):
        # 38;5;7 の7は色番号で、反転ではない
        line = "\x1b[38;5;7mxy\x1b[48;2;7;7;7mz\x1b[7;38;5;236mw\x1b[m"
        self.assertEqual(td.reversed_runs(line), [(1, 4, 4, "w")])

    def test_reversed_run_ends_at_line_end(self):
        self.assertEqual(td.reversed_runs("\x1b[7mab\nc"), [(1, 1, 2, "ab")])


@unittest.skipUnless(shutil.which("tmux"), "tmuxが無い")
class WithTmux(unittest.TestCase):
    socket = f"tuidrive-test-{os.getpid()}"

    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, SCRIPT, "--socket", self.socket, "--delay", "0.1", *args],
            capture_output=True,
            text=True,
        )

    def tearDown(self):
        self.run_cli("stop")

    def test_mouse_reaches_the_app(self):
        # cat -vは受け取った制御列を^[として画面に返す
        self.run_cli("start", "--size", "80x10", "--wait", "0.5", "--", "cat", "-v")
        self.run_cli("click", "5", "3", "--right")
        time.sleep(0.2)
        screen = self.run_cli("screen").stdout
        self.assertIn("^[[<2;5;3M^[[<2;5;3m", screen)
        # 「^[[」の3文字の後なので4桁目
        self.assertEqual(self.run_cli("find", "<2;5;3M").stdout.strip(), "4 1")

    def test_clipboard_and_alive(self):
        # OSC 52で「hi」を送り、少し待って終わるアプリ
        app = "printf '\\033]52;c;aGk=\\007'; sleep 1"
        self.run_cli("start", "--size", "80x10", "--wait", "0.5", "--", "sh", "-c", app)
        self.assertEqual(self.run_cli("clipboard").stdout, "hi")
        self.assertEqual(self.run_cli("alive").returncode, 0)
        time.sleep(1.2)
        result = self.run_cli("alive")
        self.assertEqual((result.returncode, result.stdout.strip()), (1, "終了"))


if __name__ == "__main__":
    unittest.main()
