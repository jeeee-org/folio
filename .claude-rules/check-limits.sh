#!/usr/bin/env bash
# 常時ロードされるファイルのサイズ上限を機械判定する（共通ルール§2）。
#   check-limits.sh [PJルート]     省略時はカレントディレクトリ
# 上限は 1024 系。環境変数か PJ の .claude/limits.env で上書きできる:
#   CR_LIMIT_GLOBAL=14336 CR_LIMIT_PJ=6144 CR_LIMIT_PROGRESS_BYTES=12288 CR_LIMIT_PROGRESS_LINES=60
# 各行に残量を出す。残りが CR_WARN_MARGIN_BYTES（既定 512B）/ CR_WARN_MARGIN_LINES（既定 5行）
# を切ったら △ を付けて知らせる（超過ではないので exit は 0 のまま）。上限まで使い切る
# 設計のファイルほど「あと何バイト入るか」が見えないと、次の1ルールで初めて溢れに気づく。
# 超過が1件でもあれば exit 1。
set -uo pipefail

PJ="${1:-$PWD}"
# PJ ごとの上限上書き（共通ルール§2「PJ の AGENTS.md で上書き可」の機械可読版）。
# 例: echo 'CR_LIMIT_PJ=9216' > .claude/limits.env
[ -f "$PJ/.claude/limits.env" ] && . "$PJ/.claude/limits.env"
CC="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
CX="${CODEX_HOME:-$HOME/.codex}"
G="${CR_LIMIT_GLOBAL:-14336}"
P="${CR_LIMIT_PJ:-6144}"
PB="${CR_LIMIT_PROGRESS_BYTES:-12288}"
PL="${CR_LIMIT_PROGRESS_LINES:-60}"
MB="${CR_WARN_MARGIN_BYTES:-512}"
ML="${CR_WARN_MARGIN_LINES:-5}"
NG=0
TIGHT=0

chk() { # ラベル ファイル 上限 単位
  local label="$1" f="$2" lim="$3" unit="$4" val left margin mark
  [ -f "$f" ] || return 0
  if [ "$unit" = 行 ]; then val=$(wc -l < "$f"); margin="$ML"; else val=$(wc -c < "$f"); margin="$MB"; fi
  val=$((val))
  left=$((lim-val))
  if [ "$val" -gt "$lim" ]; then
    printf '⚠ %-26s %7d%s / %d%s  (超過 %d%s)\n' "$label" "$val" "$unit" "$lim" "$unit" "$((-left))" "$unit"
    NG=1
  else
    mark=✓
    if [ "$left" -lt "$margin" ]; then mark=△; TIGHT=1; fi
    printf '%s %-26s %7d%s / %d%s  (残り %d%s)\n' "$mark" "$label" "$val" "$unit" "$lim" "$unit" "$left" "$unit"
  fi
}

echo "— グローバル（全セッションでロード）"
chk "${CC/#$HOME/\~}/CLAUDE.md" "$CC/CLAUDE.md" "$G" B
chk "${CX/#$HOME/\~}/AGENTS.md" "$CX/AGENTS.md" "$G" B

# 共通ルールをPJへ書き込んだ形（tools/embed-rules.py）では、マーカー間はグローバルの上限で、
# 外のPJ固有の分だけをPJの上限で測る（まとめて測ると書き込んだだけで必ず超過する）。
EMBED=0
chk_pj() { # ラベル ファイル
  local label="$1" f="$2" tmp
  [ -f "$f" ] || return 0
  if grep -q 'claude-rules:embed:begin' "$f"; then
    EMBED=1
    tmp="$(mktemp -d)"
    awk '/claude-rules:embed:begin/{f=1} f{print} /claude-rules:embed:end/{f=0}' "$f" > "$tmp/common"
    awk '/claude-rules:embed:begin/{f=1} !f{print} /claude-rules:embed:end/{f=0}' "$f" > "$tmp/own"
    chk "$label（共通ルール）" "$tmp/common" "$G" B
    chk "$label（PJ固有）" "$tmp/own" "$P" B
    rm -rf "$tmp"
  else
    chk "$label" "$f" "$P" B
  fi
}

echo "— PJ: ${PJ/#$HOME/\~}"
chk_pj "CLAUDE.md" "$PJ/CLAUDE.md"
chk_pj "AGENTS.md" "$PJ/AGENTS.md"
chk "PROGRESS.md" "$PJ/PROGRESS.md" "$PB" B
chk "PROGRESS.md (行数)" "$PJ/PROGRESS.md" "$PL" 行

if [ "$EMBED" = 1 ] && { grep -qs 'claude-rules:begin' "$CC/CLAUDE.md" || grep -qs 'codex-rules:begin' "$CX/AGENTS.md"; }; then
  echo "△ 共通ルールがグローバルとこのPJの両方にある（このPCでは二重に読まれる。配る先では問題ない）"
fi

if [ "$NG" -ne 0 ]; then
  echo "→ 超過あり。\"cut bytes, not meaning\" で圧縮する（畳める記録は checkpoint へ）。"
elif [ "$TIGHT" -ne 0 ]; then
  echo "→ △ は残りわずか。次に1ルール足す前に、畳める記録を checkpoint へ逃がす場所を先に作る。"
fi
exit "$NG"
