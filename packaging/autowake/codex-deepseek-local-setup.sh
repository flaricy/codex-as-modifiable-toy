#!/usr/bin/env bash
# Codex DeepSeek local setup — matches this machine's auth.command layout
# (not DeepSeek's official experimental_bearer_token script).
#
# Run:
#   ./codex-deepseek-local-setup.sh
#   ./codex-deepseek-local-setup.sh --max-concurrent-threads 5
#   CODEX_HOME=~/.our_codex ./codex-deepseek-local-setup.sh
#
# Menu:
#   1 = deepseek-v4-flash
#   2 = deepseek-v4-pro
#   3 = restore pre-install backup
#
# Default CODEX_HOME is ~/.our_codex (for the bundled ./codex wrapper).
# Override with: CODEX_HOME=/path/to/home ./codex-deepseek-local-setup.sh
#
# Differences from https://cdn.deepseek.com/api-docs/codex-deepseek-setup.sh :
#   - targets ~/.our_codex by default (not ~/.codex)
#   - stores API key via auth.command (set-deepseek-key), never in config.toml
#   - disables OpenAI auth and namespace-wrapped tools for the DeepSeek provider
#   - writes [agents].max_concurrent_threads_per_session (default 5)
#   - forces models.json deepseek-v4-flash.supports_search_tool = false
#   - leaves plugins / marketplaces untouched

set -uo pipefail

{
trap 'printf "\n已取消。\n"; exit 130' INT

SCRIPT_VERSION="1.2.0"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FLASH_SLUG="deepseek-v4-flash"
PRO_SLUG="deepseek-v4-pro"
PROVIDER_ID="deepseek"
DEFAULT_BASE_URL="https://api.deepseek.com/"
BACKUP_DIRNAME="backup-deepseek-local"
OFFICIAL_SETUP_URL="https://cdn.deepseek.com/api-docs/codex-deepseek-setup.sh"

MAX_CONCURRENT_THREADS=5
MODEL_SLUG=''

# ---------------------------------------------------------------- output

if [ -t 1 ]; then
  C_RST=$'\033[0m'; C_B=$'\033[1m'; C_RED=$'\033[31m'
  C_GRN=$'\033[32m'; C_YEL=$'\033[33m'; C_DIM=$'\033[2m'
else
  C_RST=''; C_B=''; C_RED=''; C_GRN=''; C_YEL=''; C_DIM=''
fi

info() { printf '%s\n' "$*"; }
ok()   { printf '%s✓%s %s\n' "$C_GRN" "$C_RST" "$*"; }
warn() { printf '%s!%s %s\n' "$C_YEL" "$C_RST" "$*"; }
die()  { printf '\n%s✗ %s%s\n' "$C_RED" "$*" "$C_RST" >&2; exit 1; }
head1(){ printf '\n%s%s%s\n' "$C_B" "$*" "$C_RST"; }

usage() {
  cat <<EOF
Codex DeepSeek local setup  v$SCRIPT_VERSION

Usage:
  $(basename "$0") [--max-concurrent-threads N]

Options:
  --max-concurrent-threads N   Write [agents].max_concurrent_threads_per_session
                               (default: 5; must be a positive integer)
  -h, --help                   Show this help

After launch, pick a menu item:
  1 = use $FLASH_SLUG
  2 = use $PRO_SLUG
  3 = restore default config from local backup
EOF
}

# ---------------------------------------------------------------- args

while [ $# -gt 0 ]; do
  case "$1" in
    --max-concurrent-threads)
      [ $# -ge 2 ] || die "缺少 --max-concurrent-threads 的值"
      MAX_CONCURRENT_THREADS="$2"
      shift 2
      ;;
    --max-concurrent-threads=*)
      MAX_CONCURRENT_THREADS="${1#*=}"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "未知参数：$1
$(usage)"
      ;;
  esac
done

case "$MAX_CONCURRENT_THREADS" in
  ''|*[!0-9]*) die "--max-concurrent-threads 必须是正整数，收到：$MAX_CONCURRENT_THREADS" ;;
esac
[ "$MAX_CONCURRENT_THREADS" -ge 1 ] || die "--max-concurrent-threads 必须 ≥ 1"

# ---------------------------------------------------------------- tty input

read_tty() {
  local __var="$1" __prompt="$2" __ans='' __got=1
  if [ ! -t 0 ]; then
    printf '%s' "$__prompt"
    if IFS= read -r __ans; then __got=0; fi
  fi
  if [ "$__got" -ne 0 ] && [ -r /dev/tty ]; then
    printf '%s' "$__prompt" > /dev/tty
    IFS= read -r __ans < /dev/tty || __ans=''
  fi
  eval "$__var=\$__ans"
}

# ---------------------------------------------------------------- paths
# The bundled ./codex launcher reads ~/.our_codex by default.
# Official / other installs can still override: CODEX_HOME=~/.codex ...

DEFAULT_CODEX_HOME="$HOME/.our_codex"
CODEX_HOME_DIR="${CODEX_HOME:-$DEFAULT_CODEX_HOME}"
CONFIG_PATH="$CODEX_HOME_DIR/config.toml"
MODELS_PATH="$CODEX_HOME_DIR/models.json"
BACKUP_DIR="$CODEX_HOME_DIR/$BACKUP_DIRNAME"
BACKUP_CONFIG="$BACKUP_DIR/config.toml"
BACKUP_MODELS="$BACKUP_DIR/models.json"
MANIFEST="$BACKUP_DIR/manifest.txt"
BIN_DIR="$CODEX_HOME_DIR/bin"
READ_KEY_CMD="$BIN_DIR/read-deepseek-key"
SET_KEY_CMD="$BIN_DIR/set-deepseek-key"
KEY_FILE="$CODEX_HOME_DIR/secrets/deepseek_api_key"

# Prefer ~ form for the default our_codex home; absolute path otherwise.
if [ "$CODEX_HOME_DIR" = "$HOME/.our_codex" ]; then
  CATALOG_VALUE="~/.our_codex/models.json"
elif [ "$CODEX_HOME_DIR" = "$HOME/.codex" ]; then
  CATALOG_VALUE="~/.codex/models.json"
else
  CATALOG_VALUE="$MODELS_PATH"
fi

ensure_home_layout() {
  mkdir -p "$CODEX_HOME_DIR" "$BIN_DIR" "$CODEX_HOME_DIR/secrets" || die "无法创建 $CODEX_HOME_DIR"
  chmod 700 "$CODEX_HOME_DIR/secrets" 2>/dev/null || true

  # Seed key helpers from this bundle or an existing Codex home.
  local src
  for src in "$SCRIPT_DIR/codex-resources" "$HOME/.codex/bin" "$HOME/.our_codex/bin"; do
    [ -d "$src" ] || continue
    for f in read-deepseek-key set-deepseek-key; do
      if [ ! -x "$BIN_DIR/$f" ] && [ -f "$src/$f" ]; then
        cp -a "$src/$f" "$BIN_DIR/$f"
        ok "已安装 $BIN_DIR/$f"
      fi
    done
  done
}

find_python() {
  local cand
  for cand in python3 python; do
    if command -v "$cand" >/dev/null 2>&1; then
      printf '%s' "$cand"
      return 0
    fi
  done
  return 1
}

# ---------------------------------------------------------------- restore

do_restore() {
  head1 "恢复安装前配置（删除本脚本写入的 DeepSeek 本地配置）"
  [ -d "$BACKUP_DIR" ] || die "未找到备份目录：$BACKUP_DIR
没有可还原的内容——可能尚未用本脚本安装过，或已经还原过了。"

  local had_config=1 had_models=0
  if [ -f "$MANIFEST" ]; then
    if grep -q '^original_config_existed=0$' "$MANIFEST" 2>/dev/null; then
      had_config=0
    fi
    if grep -q '^original_models_existed=1$' "$MANIFEST" 2>/dev/null; then
      had_models=1
    fi
  fi
  [ -f "$BACKUP_MODELS" ] && had_models=1

  local n=1
  info ""
  info "将执行以下操作："
  if [ "$had_config" -eq 1 ]; then
    [ -f "$BACKUP_CONFIG" ] || die "备份已损坏：缺少 $BACKUP_CONFIG"
    info "  $n. 用备份恢复 config.toml"; n=$((n+1))
    info "     ${C_YEL}（安装之后对该文件做的所有修改都会丢失）${C_RST}"
  else
    info "  $n. 删除 $CONFIG_PATH（安装前不存在）"; n=$((n+1))
  fi
  if [ "$had_models" -eq 1 ]; then
    info "  $n. 用备份恢复 models.json"; n=$((n+1))
  else
    info "  $n. 删除 $MODELS_PATH（安装前不存在）"; n=$((n+1))
  fi
  info "  $n. 删除备份目录 $BACKUP_DIR"
  info ""
  info "${C_DIM}不会动 secrets/、plugins、或 ~/.codex/bin 下的 key 工具。${C_RST}"
  info ""

  local ans=''
  read_tty ans "确认还原? 输入 y 继续，其它任意键取消: "
  case "$ans" in
    y|Y|yes|YES) ;;
    *) info "已取消，未做任何修改。"; exit 0 ;;
  esac

  if [ "$had_config" -eq 1 ]; then
    cp "$BACKUP_CONFIG" "$CONFIG_PATH" || die "恢复 config.toml 失败"
    ok "config.toml 已恢复"
  else
    rm -f "$CONFIG_PATH"
    ok "config.toml 已删除"
  fi

  if [ "$had_models" -eq 1 ]; then
    cp "$BACKUP_MODELS" "$MODELS_PATH" || die "恢复 models.json 失败"
    ok "models.json 已恢复"
  else
    rm -f "$MODELS_PATH"
    ok "models.json 已删除"
  fi

  rm -rf "$BACKUP_DIR"
  ok "备份目录已清理"
  info ""
  ok "还原完成。"
  exit 0
}

# ---------------------------------------------------------------- TOML helpers (official-script style)

_depth=0
_mlstate=''

_scan_line() {
  local line="$1"
  local n=${#line} i=0 c c3 instr=''
  while [ "$i" -lt "$n" ]; do
    c="${line:$i:1}"
    if [ -n "$_mlstate" ]; then
      c3="${line:$i:3}"
      if [ "$_mlstate" = basic ] && [ "$c3" = '"""' ]; then
        _mlstate=''; i=$((i+3)); continue
      fi
      if [ "$_mlstate" = literal ] && [ "$c3" = "'''" ]; then
        _mlstate=''; i=$((i+3)); continue
      fi
      if [ "$_mlstate" = basic ] && [ "$c" = '\' ]; then i=$((i+2)); continue; fi
      i=$((i+1)); continue
    fi
    if [ -n "$instr" ]; then
      if [ "$instr" = basic ]; then
        if [ "$c" = '\' ]; then i=$((i+2)); continue; fi
        [ "$c" = '"' ] && instr=''
      else
        [ "$c" = "'" ] && instr=''
      fi
      i=$((i+1)); continue
    fi
    c3="${line:$i:3}"
    if [ "$c3" = '"""' ]; then _mlstate=basic; i=$((i+3)); continue; fi
    if [ "$c3" = "'''" ]; then _mlstate=literal; i=$((i+3)); continue; fi
    case "$c" in
      '#') return 0 ;;
      '"') instr=basic ;;
      "'") instr=literal ;;
      '[') _depth=$((_depth+1)) ;;
      ']') [ "$_depth" -gt 0 ] && _depth=$((_depth-1)) ;;
    esac
    i=$((i+1))
  done
  return 0
}

_trim() {
  local s="$1"
  s="${s#"${s%%[![:space:]]*}"}"
  s="${s%"${s##*[![:space:]]}"}"
  printf '%s' "$s"
}

_key_of() {
  local l k
  l=$(_trim "$1")
  case "$l" in
    '#'*|'') printf ''; return ;;
    *=*) ;;
    *) printf ''; return ;;
  esac
  k=$(_trim "${l%%=*}")
  k="${k#\"}"; k="${k%\"}"
  k="${k#\'}"; k="${k%\'}"
  printf '%s' "$k"
}

_val_of() {
  local l="$1"
  case "$l" in
    *=*) printf '%s' "$(_trim "${l#*=}")" ;;
    *) printf '' ;;
  esac
}

_in_list() {
  local needle="$1"; shift
  local x
  for x in $*; do
    [ "$x" = "$needle" ] && return 0
  done
  return 1
}

TARGET_KEYS="model model_provider forced_login_method model_reasoning_effort model_catalog_json review_model"
DEL_A="profile oss_provider openai_base_url preferred_auth_method"
DEL_B="model_context_window model_auto_compact_token_limit model_auto_compact_token_limit_scope base_instructions model_instructions_file compact_prompt experimental_compact_prompt_file service_tier model_verbosity model_reasoning_summary plan_mode_reasoning_effort experimental_use_unified_exec_tool"

target_value_for() {
  case "$1" in
    model)                   printf '%s' "\"$MODEL_SLUG\"" ;;
    model_provider)          printf '%s' "\"$PROVIDER_ID\"" ;;
    forced_login_method)     printf '%s' '"api"' ;;
    model_reasoning_effort)  printf '%s' '"high"' ;;
    model_catalog_json)      printf '%s' "\"$CATALOG_VALUE\"" ;;
    review_model)            printf '%s' "\"$MODEL_SLUG\"" ;;
  esac
}

# ---------------------------------------------------------------- models.json

ensure_models_json() {
  local py tmp_src
  py=$(find_python) || die "需要 python3 来写入 / 校验 models.json"

  tmp_src=$(mktemp)
  if [ -f "$MODELS_PATH" ] && grep -q "\"$FLASH_SLUG\"" "$MODELS_PATH" 2>/dev/null \
      && grep -q "\"$PRO_SLUG\"" "$MODELS_PATH" 2>/dev/null; then
    cp "$MODELS_PATH" "$tmp_src" || die "无法读取现有 models.json"
    ok "以现有 models.json 为模板"
  else
    info "从官方 setup 脚本提取 models.json 模板…"
    local official
    official=$(mktemp)
    if ! curl -fsSL "$OFFICIAL_SETUP_URL" -o "$official"; then
      rm -f "$official" "$tmp_src"
      die "下载官方脚本失败：$OFFICIAL_SETUP_URL"
    fi
    if ! "$py" - "$official" "$tmp_src" <<'PY'
import re, sys, json
text = open(sys.argv[1], encoding='utf-8').read()
m = re.search(r"cat > \"\$TMP_MODELS\" <<'CODEX_MODELS_JSON'\n(.*?)\nCODEX_MODELS_JSON", text, re.S)
if not m:
    raise SystemExit('models.json heredoc not found in official script')
data = json.loads(m.group(1))
json.dump(data, open(sys.argv[2], 'w', encoding='utf-8'), ensure_ascii=False, indent=2)
open(sys.argv[2], 'a', encoding='utf-8').write('\n')
PY
    then
      rm -f "$official" "$tmp_src"
      die "无法从官方脚本提取 models.json"
    fi
    rm -f "$official"
    ok "已提取官方 models.json 模板"
  fi

  local tmp_out models_rc
  tmp_out=$(mktemp)
  "$py" - "$tmp_src" "$tmp_out" "$FLASH_SLUG" "$PRO_SLUG" <<'PY'
import json, sys
src, dst, flash, pro = sys.argv[1:5]
data = json.load(open(src, encoding='utf-8'))
models = data.get('models')
if not isinstance(models, list):
    raise SystemExit('models must be a list')
by_slug = {m.get('slug'): m for m in models if isinstance(m, dict)}
if flash not in by_slug or pro not in by_slug:
    raise SystemExit(f'missing required slugs: need {flash} and {pro}')
by_slug[flash]['supports_search_tool'] = False
json.dump(data, open(dst, 'w', encoding='utf-8'), ensure_ascii=False, indent=2)
open(dst, 'a', encoding='utf-8').write('\n')
PY
  models_rc=$?
  if [ "$models_rc" -ne 0 ]; then
    rm -f "$tmp_src" "$tmp_out"
    die "生成 models.json 失败"
  fi
  rm -f "$tmp_src"
  mv "$tmp_out" "$MODELS_PATH" || die "写入 models.json 失败"
  ok "models.json 已写入（$FLASH_SLUG.supports_search_tool = false）"
}

# ---------------------------------------------------------------- config rewrite

rewrite_config() {
  local ORIG_EXISTED=1
  NLINES=0
  LINES=()
  if [ -f "$CONFIG_PATH" ]; then
    while IFS= read -r __l || [ -n "$__l" ]; do
      LINES[$NLINES]="$__l"
      NLINES=$((NLINES+1))
    done < "$CONFIG_PATH"
  else
    ORIG_EXISTED=0
  fi

  OUT=()
  NOUT=0
  INS_AT=0
  REPORT=()
  NREPORT=0
  SEEN=' '
  AGENTS_SEEN=0

  out_add() { OUT[$NOUT]="$1"; NOUT=$((NOUT+1)); }
  rep_add() { REPORT[$NREPORT]="$1"; NREPORT=$((NREPORT+1)); }

  IDX=0
  _consumed_n=0
  consume_block() {
    _consumed_n=0
    while [ "$IDX" -lt "$NLINES" ]; do
      _scan_line "${LINES[$IDX]-}"
      IDX=$((IDX+1))
      _consumed_n=$((_consumed_n+1))
      if [ -z "$_mlstate" ] && [ "$_depth" -eq 0 ]; then break; fi
    done
  }

  truncate_val() {
    local v="$1"
    if [ "${#v}" -gt 58 ]; then printf '%s…' "${v:0:58}"; else printf '%s' "$v"; fi
  }

  _depth=0
  _mlstate=''
  CUR_SECTION=''
  SKIP_SECTION=0

  while [ "$IDX" -lt "$NLINES" ]; do
    line="${LINES[$IDX]-}"
    trimmed=$(_trim "$line")

    is_header=0
    if [ -z "$_mlstate" ] && [ "$_depth" -eq 0 ]; then
      case "$trimmed" in '['*) is_header=1 ;; esac
    fi

    if [ "$is_header" -eq 1 ]; then
      hdr="$trimmed"
      hdr="${hdr#[}"; hdr="${hdr#[}"
      hdr="${hdr%%]*}"
      hdr=$(_trim "$hdr")
      hdr=$(printf '%s' "$hdr" | tr -d '"'"'")
      CUR_SECTION="$hdr"
      SKIP_SECTION=0

      case "$hdr" in
        "model_providers.$PROVIDER_ID"|"model_providers.$PROVIDER_ID".*)
          SKIP_SECTION=1
          rep_add "删除旧的 [$hdr]（将以本地 auth.command 配置重写）"
          ;;
        profiles|profiles.*)
          SKIP_SECTION=1
          rep_add "删除 [$hdr]  ← profile 会遮蔽 model / model_provider"
          ;;
        agents)
          # keep section header; rewrite max_concurrent_threads_per_session below
          AGENTS_SEEN=1
          ;;
        plugins.*|marketplaces.*)
          # intentionally untouched
          ;;
      esac

      _scan_line "$line"
      IDX=$((IDX+1))
      [ "$SKIP_SECTION" -eq 0 ] && out_add "$line"
      continue
    fi

    if [ -n "$CUR_SECTION" ]; then
      if [ "$SKIP_SECTION" -eq 1 ]; then
        _scan_line "$line"; IDX=$((IDX+1)); continue
      fi
      k=$(_key_of "$line")
      if [ "$CUR_SECTION" = agents ] && [ "$k" = max_concurrent_threads_per_session ]; then
        oldv=$(_val_of "$trimmed")
        consume_block
        out_add "max_concurrent_threads_per_session = $MAX_CONCURRENT_THREADS"
        AGENTS_SEEN=2
        if [ "$oldv" != "$MAX_CONCURRENT_THREADS" ]; then
          rep_add "改写 [agents].max_concurrent_threads_per_session: $(truncate_val "$oldv") → $MAX_CONCURRENT_THREADS"
        fi
        continue
      fi
      if [ "$k" = wire_api ]; then
        v=$(_val_of "$trimmed")
        case "$v" in
          '"chat"'*|"'chat'"*)
            indent="${line%%[![:space:]]*}"
            out_add "${indent}wire_api = \"responses\""
            rep_add "修正 [$CUR_SECTION] 的 wire_api: \"chat\" → \"responses\""
            _scan_line "$line"; IDX=$((IDX+1)); continue
            ;;
        esac
      fi
      out_add "$line"
      _scan_line "$line"
      IDX=$((IDX+1))
      continue
    fi

    # leading area
    k=$(_key_of "$line")

    if [ -n "$k" ] && _in_list "$k" "$TARGET_KEYS"; then
      oldv=$(_val_of "$trimmed")
      newv=$(target_value_for "$k")
      consume_block
      out_add "$k = $newv"
      INS_AT=$NOUT
      SEEN="$SEEN$k "
      if [ "$oldv" != "$newv" ]; then
        rep_add "改写 $k: $(truncate_val "$oldv") → $newv"
      fi
      continue
    fi

    if [ -n "$k" ] && _in_list "$k" "$DEL_A $DEL_B"; then
      oldv=$(_val_of "$trimmed")
      consume_block
      rep_add "删除 $k = $(truncate_val "$oldv")"
      continue
    fi

    out_add "$line"
    [ -n "$k" ] && INS_AT=$NOUT
    _scan_line "$line"
    IDX=$((IDX+1))
  done

  MISSING=''
  for k in $TARGET_KEYS; do
    case "$SEEN" in
      *" $k "*) ;;
      *) MISSING="$MISSING$k " ;;
    esac
  done

  TMP_CONFIG="$CONFIG_PATH.deepseek-local-tmp.$$"
  : > "$TMP_CONFIG" || die "无法写入临时文件：$TMP_CONFIG"

  i=0
  while [ "$i" -lt "$NOUT" ]; do
    if [ "$i" -eq "$INS_AT" ] && [ -n "$MISSING" ]; then
      for k in $MISSING; do
        printf '%s = %s\n' "$k" "$(target_value_for "$k")" >> "$TMP_CONFIG"
        rep_add "新增 $k = $(target_value_for "$k")"
      done
      MISSING=''
      case "$(_trim "${OUT[$i]}")" in
        '['*) printf '\n' >> "$TMP_CONFIG" ;;
      esac
    fi
    printf '%s\n' "${OUT[$i]}" >> "$TMP_CONFIG"
    i=$((i+1))
  done
  if [ -n "$MISSING" ]; then
    for k in $MISSING; do
      printf '%s = %s\n' "$k" "$(target_value_for "$k")" >> "$TMP_CONFIG"
      rep_add "新增 $k = $(target_value_for "$k")"
    done
  fi

  # Ensure [agents] max_concurrent_threads_per_session
  if [ "$AGENTS_SEEN" -eq 0 ]; then
    {
      printf '\n[agents]\n'
      printf 'max_concurrent_threads_per_session = %s\n' "$MAX_CONCURRENT_THREADS"
    } >> "$TMP_CONFIG"
    rep_add "新增 [agents].max_concurrent_threads_per_session = $MAX_CONCURRENT_THREADS"
  elif [ "$AGENTS_SEEN" -eq 1 ]; then
    # section existed but key missing — append key by rewriting through python
    local py tmp2 agents_rc
    py=$(find_python) || die "需要 python3 来补全 [agents] 配置"
    tmp2=$(mktemp)
    "$py" - "$TMP_CONFIG" "$tmp2" "$MAX_CONCURRENT_THREADS" <<'PY'
from pathlib import Path
import sys
src, dst, n = sys.argv[1], sys.argv[2], sys.argv[3]
lines = Path(src).read_text(encoding='utf-8').splitlines(keepends=True)
out = []
in_agents = False
inserted = False
for line in lines:
    stripped = line.strip()
    if stripped.startswith('[') and stripped.endswith(']'):
        if in_agents and not inserted:
            out.append(f'max_concurrent_threads_per_session = {n}\n')
            inserted = True
        in_agents = (stripped == '[agents]')
        out.append(line)
        continue
    out.append(line)
if in_agents and not inserted:
    out.append(f'max_concurrent_threads_per_session = {n}\n')
    inserted = True
if not inserted:
    out.append('\n[agents]\n')
    out.append(f'max_concurrent_threads_per_session = {n}\n')
Path(dst).write_text(''.join(out), encoding='utf-8')
PY
    agents_rc=$?
    if [ "$agents_rc" -ne 0 ]; then
      rm -f "$TMP_CONFIG" "$tmp2"
      die "补全 [agents] 失败"
    fi
    mv "$tmp2" "$TMP_CONFIG"
    rep_add "新增 [agents].max_concurrent_threads_per_session = $MAX_CONCURRENT_THREADS"
  fi

  {
    printf '\n[model_providers.%s]\n' "$PROVIDER_ID"
    printf 'name = "%s"\n' "$PROVIDER_ID"
    printf 'base_url = "%s"\n' "$DEFAULT_BASE_URL"
    printf 'wire_api = "responses"\n'
    printf 'requires_openai_auth = false\n'
    printf 'namespace_tools = false\n'
    printf '\n'
    printf '# API key is NOT stored in this file. Add it with:\n'
    printf '#   %s\n' "$SET_KEY_CMD"
    printf '[model_providers.%s.auth]\n' "$PROVIDER_ID"
    printf 'command = "%s"\n' "$READ_KEY_CMD"
    printf 'timeout_ms = 5000\n'
    printf 'refresh_interval_ms = 0\n'
  } >> "$TMP_CONFIG"

  local py rc
  py=$(find_python || true)
  if [ -n "$py" ]; then
    "$py" - "$TMP_CONFIG" "$MODEL_SLUG" "$MAX_CONCURRENT_THREADS" <<'PY' 2>/dev/null
import sys
try:
    import tomllib
except ImportError:
    sys.exit(3)
with open(sys.argv[1], 'rb') as f:
    c = tomllib.load(f)
assert c.get('model') == sys.argv[2]
assert c.get('model_provider') == 'deepseek'
assert 'preferred_auth_method' not in c
assert c.get('model_catalog_json')
assert 'deepseek' in c.get('model_providers', {})
provider = (c.get('model_providers') or {}).get('deepseek', {})
assert provider.get('requires_openai_auth') is False
assert provider.get('namespace_tools') is False
agents = c.get('agents') or {}
assert int(agents.get('max_concurrent_threads_per_session', -1)) == int(sys.argv[3])
auth = provider.get('auth') or {}
assert auth.get('command'), 'auth.command missing'
assert 'experimental_bearer_token' not in provider
PY
    rc=$?
    if [ "$rc" -eq 0 ]; then
      ok "config.toml 通过 TOML 校验"
    elif [ "$rc" -eq 3 ]; then
      warn "当前 Python 无 tomllib，跳过严格 TOML 校验"
    else
      rm -f "$TMP_CONFIG"
      die "生成的 config.toml 未通过校验，已中止（原文件未改）。"
    fi
  fi

  mv "$TMP_CONFIG" "$CONFIG_PATH" || die "写入 config.toml 失败"
  ok "config.toml 已更新"

  if [ "$NREPORT" -gt 0 ]; then
    head1 "对原有配置的改动（共 $NREPORT 项）"
    i=0
    while [ "$i" -lt "$NREPORT" ]; do
      printf '  • %s\n' "${REPORT[$i]}"
      i=$((i+1))
    done
  fi

  # expose report for manifest
  _LAST_REPORT_N=$NREPORT
  _LAST_REPORT=("${REPORT[@]+"${REPORT[@]}"}")
}

update_existing_install() {
  head1 "更新已有本地 DeepSeek 配置 → $MODEL_SLUG"
  info "${C_DIM}检测到本脚本备份已就绪；将更新 model / review_model / agents 线程数，并校正 models.json。${C_RST}"

  ensure_models_json
  rewrite_config

  {
    printf 'script_version=%s\n' "$SCRIPT_VERSION"
    printf 'updated_at=%s\n' "$(date '+%Y-%m-%d %H:%M:%S')"
    printf 'model_slug=%s\n' "$MODEL_SLUG"
    printf 'max_concurrent_threads_per_session=%s\n' "$MAX_CONCURRENT_THREADS"
    printf 'catalog_value=%s\n' "$CATALOG_VALUE"
    printf 'codex_home=%s\n' "$CODEX_HOME_DIR"
  } >> "$MANIFEST" 2>/dev/null || true

  print_summary
  exit 0
}

ensure_key_helpers() {
  [ -x "$READ_KEY_CMD" ] || die "缺少可执行文件：$READ_KEY_CMD"
  [ -x "$SET_KEY_CMD" ] || die "缺少可执行文件：$SET_KEY_CMD"
}

ensure_api_key() {
  if [ -f "$KEY_FILE" ] && [ -s "$KEY_FILE" ]; then
    ok "已检测到 DeepSeek API key：$KEY_FILE"
    return 0
  fi
  warn "未找到 API key 文件：$KEY_FILE"
  info "将调用：$SET_KEY_CMD"
  info ""
  "$SET_KEY_CMD" || die "未能保存 API key"
  [ -s "$KEY_FILE" ] || die "API key 文件仍为空：$KEY_FILE"
}

print_summary() {
  head1 "当前配置"
  cat <<EOF
  model                                     = "$MODEL_SLUG"
  model_provider                            = "$PROVIDER_ID"
  forced_login_method                       = "api"
  model_reasoning_effort                    = "high"
  model_catalog_json                        = "$CATALOG_VALUE"
  [agents].max_concurrent_threads_per_session = $MAX_CONCURRENT_THREADS

  [model_providers.$PROVIDER_ID]
  base_url             = "$DEFAULT_BASE_URL"
  wire_api             = "responses"
  requires_openai_auth = false
  namespace_tools      = false
  auth.command         = "$READ_KEY_CMD"

  models.json
  $FLASH_SLUG.supports_search_tool = false
EOF
  info ""
  ok "完成。重启 Codex 后生效。"
  info "${C_DIM}plugins / marketplaces 未改动。还原：再运行本脚本选 3。${C_RST}"
}

# ---------------------------------------------------------------- main

head1 "Codex DeepSeek Local Setup  v$SCRIPT_VERSION"
info "${C_DIM}Codex 目录: $CODEX_HOME_DIR${C_RST}"
info "${C_DIM}max_concurrent_threads_per_session = $MAX_CONCURRENT_THREADS${C_RST}"

ensure_home_layout

ensure_key_helpers

info ""
info "请选择要执行的操作："
info "  ${C_B}1${C_RST}. 使用 $FLASH_SLUG"
info "  ${C_B}2${C_RST}. 使用 $PRO_SLUG"
info "  ${C_B}3${C_RST}. 恢复本脚本安装前的配置"
info ""

CHOICE=''
ATTEMPT=0
while :; do
  read_tty CHOICE "输入 1 / 2 / 3: "
  case "$CHOICE" in
    1|2|3) break ;;
  esac
  ATTEMPT=$((ATTEMPT+1))
  [ "$ATTEMPT" -ge 3 ] && die "无效选择，已退出（未修改任何文件）。"
  warn "无效输入，请输入 1、2 或 3。"
done

case "$CHOICE" in
  1) MODEL_SLUG="$FLASH_SLUG" ;;
  2) MODEL_SLUG="$PRO_SLUG" ;;
  3) do_restore ;;
esac

# Already installed by this script → update path
if [ -d "$BACKUP_DIR" ] && [ -f "$MANIFEST" ]; then
  if [ -f "$CONFIG_PATH" ] && grep -q "^\[model_providers\.$PROVIDER_ID\]" "$CONFIG_PATH" 2>/dev/null \
      && grep -q 'auth' "$CONFIG_PATH" 2>/dev/null; then
    update_existing_install
  fi
  warn "备份目录存在但当前配置不像本脚本安装结果；将继续做一次完整写入。"
fi

head1 "安装 / 重写（目标模型：${MODEL_SLUG}）"

mkdir -p "$BACKUP_DIR" || die "无法创建备份目录：$BACKUP_DIR"

ORIG_CONFIG_EXISTED=1
ORIG_MODELS_EXISTED=0
if [ -f "$CONFIG_PATH" ]; then
  cp "$CONFIG_PATH" "$BACKUP_CONFIG" || die "备份 config.toml 失败"
  ok "已备份 config.toml → $BACKUP_CONFIG"
else
  ORIG_CONFIG_EXISTED=0
  warn "未找到 config.toml，将创建新文件"
fi
if [ -f "$MODELS_PATH" ]; then
  cp "$MODELS_PATH" "$BACKUP_MODELS" || die "备份 models.json 失败"
  ORIG_MODELS_EXISTED=1
  ok "已备份 models.json → $BACKUP_MODELS"
fi

ensure_api_key
ensure_models_json
rewrite_config

{
  printf 'script_version=%s\n' "$SCRIPT_VERSION"
  printf 'installed_at=%s\n' "$(date '+%Y-%m-%d %H:%M:%S')"
  printf 'original_config_existed=%s\n' "$ORIG_CONFIG_EXISTED"
  printf 'original_models_existed=%s\n' "$ORIG_MODELS_EXISTED"
  printf 'model_slug=%s\n' "$MODEL_SLUG"
  printf 'max_concurrent_threads_per_session=%s\n' "$MAX_CONCURRENT_THREADS"
  printf 'base_url=%s\n' "$DEFAULT_BASE_URL"
  printf 'catalog_value=%s\n' "$CATALOG_VALUE"
  printf 'codex_home=%s\n' "$CODEX_HOME_DIR"
  printf 'auth_mode=command\n'
  printf 'read_key_cmd=%s\n' "$READ_KEY_CMD"
} > "$MANIFEST"

print_summary
exit 0
}
