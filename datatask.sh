#!/usr/bin/env bash
# =============================================================================
# DataTask · script de gerenciamento
#
#   setup       Instala/valida dependências, cria config com token, compila
#   run         Compila (release) e executa backend + frontend
#   dev         Executa em modo desenvolvimento (cargo run)
#   build       Apenas compila o binário release
#   token       Mostra o token configurado
#   url         Mostra a URL do painel
#   install     Instala e ativa o serviço no systemd (sudo)
#   uninstall   Remove o serviço do systemd (sudo)
#   start|stop|restart|reload|status|logs   Controle do serviço (sudo)
# =============================================================================
set -Eeuo pipefail

SCRIPT_PATH="$(readlink -f "${BASH_SOURCE[0]}")"
ROOT_DIR="${DATATASK_ROOT:-$(cd "$(dirname "$SCRIPT_PATH")" && pwd)}"
ORIG_ARGS=("$@")

SERVICE_NAME="datatask"
UNIT_FILE="/etc/systemd/system/${SERVICE_NAME}.service"
BIN_PATH="/usr/local/bin/${SERVICE_NAME}"
CONF_DIR="/etc/${SERVICE_NAME}"
CONF_FILE="${CONF_DIR}/config.toml"
DATA_DIR="/var/lib/${SERVICE_NAME}"
JOBS_DIR="${DATA_DIR}/jobs"
LOGS_DIR="${DATA_DIR}/logs"

DEFAULT_BIND="127.0.0.1"
DEFAULT_PORT="8080"

CMD=""
OPT_CONFIG=""
OPT_PORT=""
OPT_BIND=""
OPT_USER=""
OPT_JOBS_DIR=""
OPT_LOGS_DIR=""
OPT_POLL=""
OPT_TIMEOUT=""
OPT_PURGE=0
OPT_YES=0
OPT_REBUILD=0
OPT_NO_BUILD=0
OPT_DEPS=0
OPT_DRY_RUN=0
OPT_FOLLOW=1
OPT_SERVICE=0
OPT_LOCAL=0
OPT_KEEP_CONFIG=0
PROMPT=1

# ---------------------------------------------------------------- interface --
if [[ -t 1 ]]; then
  C_RESET=$'\033[0m'; C_BLUE=$'\033[38;5;75m'; C_GREEN=$'\033[38;5;42m'
  C_YELLOW=$'\033[38;5;220m'; C_RED=$'\033[38;5;203m'; C_BOLD=$'\033[1m'
else
  C_RESET=""; C_BLUE=""; C_GREEN=""; C_YELLOW=""; C_RED=""; C_BOLD=""
fi

log()  { printf '%s[datatask]%s %s\n' "$C_BLUE" "$C_RESET" "$*"; }
ok()   { printf '%s✓%s %s\n' "$C_GREEN" "$C_RESET" "$*"; }
warn() { printf '%s!%s %s\n' "$C_YELLOW" "$C_RESET" "$*" >&2; }
die()  { printf '%s✗%s %s\n' "$C_RED" "$C_RESET" "$*" >&2; exit 1; }

trap 'code=$?; if [[ "$code" -ne 141 ]]; then die "Falha inesperada na linha ${LINENO} (comando: ${BASH_COMMAND})."; fi' ERR

usage() {
  cat <<EOF
${C_BOLD}DataTask${C_RESET} · gerenciamento do agendador

${C_BOLD}Uso:${C_RESET}
  ${0##*/} <comando> [opções]

${C_BOLD}Comandos locais (sem sudo):${C_RESET}
  setup              Setup completo: dependências, config com token, pastas e build
  run, serve         Compila (release) e roda backend + frontend em primeiro plano
  dev                Roda backend + frontend em modo desenvolvimento (cargo run)
  build              Compila apenas o binário release
  token              Exibe o token configurado (serviço, se instalado)
  url                Exibe a URL do painel (serviço, se instalado)
  doctor             Diagnostica config, tokens, serviço e saúde da API

${C_BOLD}Serviço systemd (precisa de sudo):${C_RESET}
  install            Instala backend + frontend (painel embutido) como serviço e ativa
  uninstall          Para e remove o serviço (mantém dados, salvo --purge)
  start | stop | restart | reload | status
  logs               Acompanha os logs do serviço (journalctl)

${C_BOLD}Opções:${C_RESET}
  --config <arquivo>   Config TOML (local ou origem para o install)
  --port <porta>       Porta do painel (padrão: ${DEFAULT_PORT})
  --bind <endereço>    Endereço de escuta (padrão: ${DEFAULT_BIND})
  --jobs-dir <dir>     Diretório das tarefas (padrão no setup: jobs)
  --logs-dir <dir>     Diretório dos logs (padrão no setup: logs)
  --poll <segundos>    Intervalo de verificação da agenda (padrão: 1)
  --timeout <segundos> Timeout padrão dos comandos (padrão: 300)
  --user <usuário>     Usuário do serviço systemd
  --purge              No uninstall, remove também /etc/datatask e /var/lib/datatask
  --rebuild            Força recompilação
  --no-build           Não compila (exige binário existente)
  --with-deps          Instala dependências sem perguntar (Rust/build tools)
  --dry-run            No install, apenas mostra o unit file e os caminhos
  --keep-config        No install, não sobrescreve /etc/datatask/config.toml
  --service            Usa a config do serviço em token/url
  --local              Usa a config do projeto em token/url
  --no-follow          No logs, imprime as últimas linhas e sai
  -y, --yes            Responde "sim" a todas as confirmações (setup sem perguntas)
  -h, --help           Mostra esta ajuda

${C_BOLD}Exemplos:${C_RESET}
  ./${0##*/} setup
  ./${0##*/} run
  sudo ./${0##*/} install
  sudo ./${0##*/} install --port 9000 --user "$USER"
  sudo ./${0##*/} uninstall --purge
  sudo ./${0##*/} logs
EOF
}

# -------------------------------------------------------------- utilitários --
confirm() {
  [[ "$OPT_YES" -eq 1 ]] && return 0
  [[ -t 0 ]] || return 1
  local reply
  read -r -p "$1 [s/N] " reply
  [[ "$reply" =~ ^([sS]|[yY]|sim|yes)$ ]]
}

need_root() {
  [[ "$EUID" -eq 0 ]] && return 0
  if [[ -t 0 ]]; then
    warn "Este comando precisa de sudo; elevando privilégios…"
    exec sudo -- "$SCRIPT_PATH" "${ORIG_ARGS[@]}"
  fi
  die "Execute com sudo: sudo $SCRIPT_PATH ${CMD:-install}"
}

require_systemd() {
  command -v systemctl >/dev/null 2>&1 || die "systemctl não encontrado. Este recurso exige systemd."
  [[ -d /run/systemd/system ]] || die "systemd não está ativo neste sistema."
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "Comando '$1' não encontrado. $2"
}

resolve_path() {
  case "$1" in
    /*) printf '%s' "$1" ;;
    *) printf '%s/%s' "$PWD" "$1" ;;
  esac
}

new_token() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32
  else
    head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'
  fi
}

# ------------------------------------------------------------------ prompts --
# Pergunta um valor mostrando o padrão entre colchetes; Enter aceita o padrão.
ask() {
  local label="$1" default="$2" reply
  if [[ "$PROMPT" -ne 1 ]]; then
    printf '%s' "$default"
    return 0
  fi
  if [[ -n "$default" ]]; then
    printf '%s%s%s [%s%s%s]: ' "$C_BOLD" "$label" "$C_RESET" "$C_GREEN" "$default" "$C_RESET" >&2
  else
    printf '%s%s%s: ' "$C_BOLD" "$label" "$C_RESET" >&2
  fi
  IFS= read -r reply || true
  reply="${reply%$'\r'}"
  [[ -z "$reply" ]] && reply="$default"
  printf '%s' "$reply"
}

# Lê um segredo sem ecoar no terminal.
read_secret() {
  local prompt="$1" value
  printf '%s' "$prompt" >&2
  IFS= read -r -s value || true
  printf '\n' >&2
  printf '%s' "${value%$'\r'}"
}

# Senha/token da API: Enter mantém a atual ou gera uma nova.
ask_password() {
  local current="${1:-}" token confirm default_hint
  if [[ -n "${DATATASK_TOKEN:-}" ]]; then
    is_weak_token "$DATATASK_TOKEN" \
      && die "DATATASK_TOKEN precisa ter 32+ caracteres e não pode ser um placeholder."
    printf '%s' "$DATATASK_TOKEN"
    return 0
  fi
  if [[ "$PROMPT" -ne 1 ]]; then
    if [[ -n "$current" ]]; then printf '%s' "$current"; else new_token; fi
    return 0
  fi
  if [[ -n "$current" ]]; then
    default_hint="Enter mantém a atual (${current:0:4}…${current: -4})"
  else
    default_hint="mín. 32 caracteres, Enter gera automaticamente"
  fi
  while true; do
    token="$(read_secret "${C_BOLD}Senha da API${C_RESET} (${default_hint}): ")"
    if [[ -z "$token" ]]; then
      if [[ -n "$current" ]]; then printf '%s' "$current"; return 0; fi
      new_token
      return 0
    fi
    if [[ "${#token}" -lt 32 ]]; then
      warn "A senha precisa de pelo menos 32 caracteres."
      continue
    fi
    if is_weak_token "$token"; then
      warn "Esse token parece um exemplo/placeholder. Gere um aleatório (openssl rand -hex 32)."
      continue
    fi
    confirm="$(read_secret "${C_BOLD}Confirme a senha${C_RESET}: ")"
    if [[ "$token" != "$confirm" ]]; then
      warn "As senhas não conferem. Tente novamente."
      continue
    fi
    printf '%s' "$token"
    return 0
  done
}

# Retorna flag, se informada; senão pergunta com o padrão.
ask_flag() {
  local flag="$1" label="$2" default="$3"
  if [[ -n "$flag" ]]; then
    printf '%s' "$flag"
  else
    ask "$label" "$default"
  fi
}

validate_uint() { [[ "$1" =~ ^[0-9]+$ ]]; }

ask_uint() {
  local flag="$1" label="$2" default="$3" min="$4" max="$5" value
  if [[ -n "$flag" ]]; then
    validate_uint "$flag" || die "$label inválido: $flag"
    (( flag >= min && flag <= max )) || die "$label deve estar entre ${min} e ${max} (recebido: $flag)."
    value="$flag"
  else
    while true; do
      value="$(ask "$label" "$default")"
      if validate_uint "$value" && (( value >= min && value <= max )); then
        break
      fi
      warn "$label deve ser um número entre ${min} e ${max}."
      [[ "$PROMPT" -eq 1 ]] || die "$label inválido: $value"
    done
  fi
  printf '%s' "$value"
}

expand_path() {
  case "$1" in
    "~") printf '%s' "$HOME" ;;
    "~/"*) printf '%s/%s' "$HOME" "${1#\~/}" ;;
    *) printf '%s' "$1" ;;
  esac
}

config_get() {
  local file="$1" key="$2" value
  [[ -f "$file" ]] || return 0
  value="$(grep -E "^[[:space:]]*${key}[[:space:]]*=" "$file" | tail -n1 | cut -d= -f2- || true)"
  value="${value%%#*}"
  value="${value//\"/}"
  value="${value//\'/}"
  printf '%s' "$value" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

# Mantém aspas/estrutura (arrays TOML), ao contrário de config_get.
config_get_raw() {
  local file="$1" key="$2" value
  [[ -f "$file" ]] || return 0
  value="$(grep -E "^[[:space:]]*${key}[[:space:]]*=" "$file" | tail -n1 | cut -d= -f2- || true)"
  value="${value%%#*}"
  printf '%s' "$value" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

config_set_string() {
  local file="$1" key="$2" value="$3" tmp
  tmp="$(mktemp)"
  if grep -qE "^[[:space:]]*${key}[[:space:]]*=" "$file"; then
    awk -v key="$key" -v value="$value" '
      BEGIN { done = 0 }
      $0 ~ "^[[:space:]]*" key "[[:space:]]*=" && !done { print key " = \"" value "\""; done = 1; next }
      { print }
    ' "$file" > "$tmp"
    cat "$tmp" > "$file"
    rm -f "$tmp"
  else
    printf '%s = "%s"\n' "$key" "$value" >> "$file"
  fi
}

config_set_number() {
  local file="$1" key="$2" value="$3" tmp
  tmp="$(mktemp)"
  if grep -qE "^[[:space:]]*${key}[[:space:]]*=" "$file"; then
    awk -v key="$key" -v value="$value" '
      BEGIN { done = 0 }
      $0 ~ "^[[:space:]]*" key "[[:space:]]*=" && !done { print key " = " value; done = 1; next }
      { print }
    ' "$file" > "$tmp"
    cat "$tmp" > "$file"
    rm -f "$tmp"
  else
    printf '%s = %s\n' "$key" "$value" >> "$file"
  fi
}

# Escreve um valor TOML bruto (booleano, array etc.), sem aspas.
config_set_raw() {
  config_set_number "$1" "$2" "$3"
}

config_remove() {
  local file="$1" key="$2" tmp
  [[ -f "$file" ]] || return 0
  tmp="$(mktemp)"
  grep -vE "^[[:space:]]*${key}[[:space:]]*=" "$file" > "$tmp" || true
  cat "$tmp" > "$file"
  rm -f "$tmp"
}

# Lê o token, aceitando api_token_file (recomendado) ou api_token (legado).
config_token() {
  local file="$1" path
  path="$(config_get "$file" api_token_file)"
  if [[ -n "$path" ]]; then
    [[ "$path" = /* ]] || path="$(dirname "$file")/${path}"
    [[ -r "$path" ]] || return 0
    tr -d '\r\n' < "$path"
  else
    config_get "$file" api_token
  fi
}

# Considera fracos tokens vazios, curtos ou placeholders conhecidos.
is_weak_token() {
  local token="$1" lower
  [[ -z "$token" || "${#token}" -lt 32 ]] && return 0
  lower="$(printf '%s' "$token" | tr '[:upper:]' '[:lower:]')"
  case "$lower" in
    *substitua*|*changeme*|*replace-*|*troque*|*your-token*|*cole-o-token*|*example-token*) return 0 ;;
  esac
  return 1
}

write_config() {
  local file="$1" bind="$2" port="$3" jobs="$4" logs="$5"
  local poll="${6:-1}" timeout="${7:-300}" token="${8:-$(new_token)}"
  local max_jobs="${9:-4}" log_max="${10:-10485760}"
  local rate_limit="${11:-600}" auth_limit="${12:-10}"
  local auth_window="${13:-60}" auth_lockout="${14:-300}"
  local notify_timeout="${15:-10}"
  mkdir -p "$(dirname "$file")"
  local old_umask
  old_umask="$(umask)"
  umask 077
  cat > "$file" <<EOF
# DataTask · configuração gerada por datatask.sh
bind_address = "$bind"
port = $port
api_token = "$token"
jobs_dir = "$jobs"
logs_dir = "$logs"
poll_interval_seconds = $poll
command_timeout_seconds = $timeout
notification_timeout_seconds = $notify_timeout
max_concurrent_jobs = $max_jobs
log_max_bytes = $log_max
rate_limit_per_minute = $rate_limit
auth_failure_limit = $auth_limit
auth_failure_window_seconds = $auth_window
auth_lockout_seconds = $auth_lockout
tls_terminated_by_proxy = false
trusted_proxy_ips = []
trust_proxy_headers = false
audit_log_file = "logs/audit.jsonl"
history_file = "logs/history.jsonl"
backup_dir = ""
backup_interval_seconds = 86400
backup_retention = 7
backup_ssh_target = ""
backup_ssh_port = 22
backup_s3_uri = ""
backup_encryption_key_file = ""
backup_remote_timeout_seconds = 60
backup_verify_interval_seconds = 86400
backup_alert_url = ""
backup_health_report_url = ""
backup_health_report_interval_seconds = 86400
EOF
  umask "$old_umask"
}

ensure_token() {
  local file="$1" token token_file
  token="$(config_token "$file")"
  if is_weak_token "$token"; then
    token="$(new_token)"
    token_file="$(config_get "$file" api_token_file)"
    if [[ -n "$token_file" ]]; then
      [[ "$token_file" = /* ]] || token_file="$(dirname "$file")/${token_file}"
      local old_umask; old_umask="$(umask)"
      umask 077
      printf '%s\n' "$token" > "$token_file"
      umask "$old_umask"
      chmod 600 "$token_file"
    else
      config_set_string "$file" api_token "$token"
    fi
    ok "Token forte gerado em $file"
  fi
}

panel_url() {
  local file="$1" host port scheme="http"
  host="$(config_get "$file" bind_address)"
  port="$(config_get "$file" port)"
  if [[ -n "$(config_get "$file" tls_cert_file)" && -n "$(config_get "$file" tls_key_file)" ]]; then
    scheme="https"
  fi
  host="${host:-$DEFAULT_BIND}"
  port="${port:-$DEFAULT_PORT}"
  [[ "$host" == "0.0.0.0" || "$host" == "::" ]] && host="127.0.0.1"
  printf '%s://%s:%s' "$scheme" "$host" "$port"
}

http_get() {
  if command -v curl >/dev/null 2>&1; then
    if [[ "$1" == https://* ]]; then
      # Health check local; certificados self-signed são aceitos aqui.
      curl -kfsS --max-time 2 "$1"
    else
      curl -fsS --max-time 2 "$1"
    fi
  elif command -v wget >/dev/null 2>&1; then
    if [[ "$1" == https://* ]]; then
      wget -qO- --no-check-certificate --timeout=2 "$1"
    else
      wget -qO- --timeout=2 "$1"
    fi
  else
    return 1
  fi
}

ensure_build_tools() {
  if command -v cc >/dev/null 2>&1 || command -v gcc >/dev/null 2>&1; then
    return 0
  fi
  warn "Compilador C não encontrado (necessário para linkar o binário)."
  if [[ "$OPT_DEPS" -eq 1 || "$OPT_YES" -eq 1 ]] || confirm "Instalar build-essential via apt agora?"; then
    if command -v apt-get >/dev/null 2>&1; then
      if [[ "$EUID" -eq 0 ]]; then
        apt-get update && apt-get install -y build-essential
      elif command -v sudo >/dev/null 2>&1; then
        sudo apt-get update && sudo apt-get install -y build-essential
      else
        die "Instale build-essential manualmente."
      fi
      ok "build-essential instalado."
    else
      die "Instale um compilador C (build-essential) e tente novamente."
    fi
  else
    die "Instale build-essential e rode novamente."
  fi
}

ensure_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    return 0
  fi
  [[ "$EUID" -eq 0 ]] && die "Rust não encontrado para root. Instale com rustup para o usuário do serviço e rode novamente."
  warn "Rust/cargo não encontrado."
  if [[ "$OPT_DEPS" -eq 1 ]] || confirm "Instalar Rust via rustup agora?"; then
    require_cmd curl "É necessário para baixar o rustup."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    # shellcheck disable=SC1091
    [[ -f "$HOME/.cargo/env" ]] && . "$HOME/.cargo/env"
    export PATH="$HOME/.cargo/bin:$PATH"
    command -v cargo >/dev/null 2>&1 || die "Instalação do Rust falhou. Abra um novo terminal e rode novamente."
    ok "Rust instalado em $HOME/.cargo"
  else
    die "Instale o Rust em https://rustup.rs e rode novamente."
  fi
}

as_user() {
  local user="$1" command="$2" home
  home="$(getent passwd "$user" | cut -d: -f6)"
  if command -v runuser >/dev/null 2>&1; then
    runuser -u "$user" -- env HOME="$home" PATH="$home/.cargo/bin:/usr/local/bin:/usr/bin:/bin" \
      bash -c "cd \"$ROOT_DIR\" && $command"
  else
    sudo -u "$user" env HOME="$home" PATH="$home/.cargo/bin:/usr/local/bin:/usr/bin:/bin" \
      bash -c "cd \"$ROOT_DIR\" && $command"
  fi
}

build_release() {
  local binary="$ROOT_DIR/target/release/datatask"
  if [[ "$OPT_NO_BUILD" -eq 1 ]]; then
    [[ -x "$binary" ]] || die "Binário ausente em $binary e --no-build foi informado."
    ok "Usando binário existente: $binary"
    return 0
  fi
  ensure_cargo
  ensure_build_tools
  log "Compilando release (cargo build --release)…"
  if [[ "$EUID" -eq 0 ]]; then
    if [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
      as_user "$SUDO_USER" "cargo build --release"
    else
      ( cd "$ROOT_DIR" && cargo build --release )
    fi
  else
    ( cd "$ROOT_DIR" && cargo build --release )
  fi
  [[ -x "$binary" ]] || die "Build concluído, mas $binary não foi encontrado."
  ok "Binário: $binary"
}

ensure_local_config() {
  local conf="${1:-$LOCAL_CONFIG}" bind port jobs logs dir
  dir="$(dirname "$conf")"
  if [[ ! -f "$conf" ]]; then
    write_config "$conf" "${OPT_BIND:-$DEFAULT_BIND}" "${OPT_PORT:-$DEFAULT_PORT}" "jobs" "logs"
    ok "Configuração criada em $conf"
  else
    [[ -n "$OPT_BIND" ]] && config_set_string "$conf" bind_address "$OPT_BIND"
    [[ -n "$OPT_PORT" ]] && config_set_number "$conf" port "$OPT_PORT"
    ensure_token "$conf"
  fi
  jobs="$(config_get "$conf" jobs_dir)"
  logs="$(config_get "$conf" logs_dir)"
  [[ "$jobs" = /* ]] || jobs="${dir}/${jobs:-jobs}"
  [[ "$logs" = /* ]] || logs="${dir}/${logs:-logs}"
  mkdir -p "$jobs" "$logs"
  chmod 600 "$conf" 2>/dev/null || true
}

# Resolve o arquivo de config local (absoluto).
LOCAL_CONFIG="$ROOT_DIR/config.toml"
resolve_options() {
  if [[ -n "$OPT_CONFIG" ]]; then
    LOCAL_CONFIG="$(resolve_path "$OPT_CONFIG")"
  fi
  if [[ -n "$OPT_PORT" ]]; then
    validate_uint "$OPT_PORT" && (( OPT_PORT >= 1 && OPT_PORT <= 65535 )) \
      || die "Porta inválida: $OPT_PORT (use 1-65535)."
  fi
  if [[ -n "$OPT_POLL" ]]; then
    validate_uint "$OPT_POLL" && (( OPT_POLL >= 1 && OPT_POLL <= 3600 )) \
      || die "--poll inválido: $OPT_POLL (use 1-3600)."
  fi
  if [[ -n "$OPT_TIMEOUT" ]]; then
    validate_uint "$OPT_TIMEOUT" && (( OPT_TIMEOUT >= 1 && OPT_TIMEOUT <= 86400 )) \
      || die "--timeout inválido: $OPT_TIMEOUT (use 1-86400)."
  fi
}

# ------------------------------------------------------------------ comandos --
cmd_setup() {
  log "Setup do DataTask em $ROOT_DIR"
  ensure_cargo
  ensure_build_tools

  local conf="$LOCAL_CONFIG" old_prompt="$PROMPT"
  [[ "$OPT_YES" -eq 1 || ! -t 0 ]] && PROMPT=0

  # Valores atuais (ou padrões) aparecem entre colchetes: Enter mantém.
  local cur_bind cur_port cur_jobs cur_logs cur_poll cur_timeout cur_token cur_token_file
  local cur_max_jobs cur_log_max cur_rate cur_auth_limit cur_auth_window cur_auth_lockout cur_notify
  local prev_tls prev_trust prev_audit prev_audit_set=0
  local prev_history prev_history_set=0
  local prev_trusted prev_trusted_set=0
  local prev_cert prev_cert_set=0 prev_key prev_key_set=0
  local prev_backup_dir prev_backup_dir_set=0 prev_backup_interval prev_backup_interval_set=0
  local prev_backup_retention prev_backup_retention_set=0
  local prev_backup_ssh prev_backup_ssh_set=0 prev_backup_port prev_backup_port_set=0
  local prev_backup_s3 prev_backup_s3_set=0 prev_backup_key prev_backup_key_set=0
  local prev_backup_timeout prev_backup_timeout_set=0
  local prev_backup_verify prev_backup_verify_set=0
  local prev_backup_alert prev_backup_alert_set=0
  local prev_backup_report prev_backup_report_set=0
  local prev_backup_report_interval prev_backup_report_interval_set=0
  cur_bind="$(config_get "$conf" bind_address)"
  cur_port="$(config_get "$conf" port)"
  cur_jobs="$(config_get "$conf" jobs_dir)"
  cur_logs="$(config_get "$conf" logs_dir)"
  cur_poll="$(config_get "$conf" poll_interval_seconds)"
  cur_timeout="$(config_get "$conf" command_timeout_seconds)"
  cur_token="$(config_token "$conf")"
  cur_token_file="$(config_get "$conf" api_token_file)"
  cur_max_jobs="$(config_get "$conf" max_concurrent_jobs)"
  cur_log_max="$(config_get "$conf" log_max_bytes)"
  cur_rate="$(config_get "$conf" rate_limit_per_minute)"
  cur_auth_limit="$(config_get "$conf" auth_failure_limit)"
  cur_auth_window="$(config_get "$conf" auth_failure_window_seconds)"
  cur_auth_lockout="$(config_get "$conf" auth_lockout_seconds)"
  cur_notify="$(config_get "$conf" notification_timeout_seconds)"
  prev_tls="$(config_get "$conf" tls_terminated_by_proxy)"
  prev_trust="$(config_get "$conf" trust_proxy_headers)"
  if grep -qE '^[[:space:]]*audit_log_file[[:space:]]*=' "$conf"; then
    prev_audit="$(config_get "$conf" audit_log_file)"
    prev_audit_set=1
  fi
  if grep -qE '^[[:space:]]*trusted_proxy_ips[[:space:]]*=' "$conf"; then
    prev_trusted="$(config_get_raw "$conf" trusted_proxy_ips)"
    prev_trusted_set=1
  fi
  if grep -qE '^[[:space:]]*history_file[[:space:]]*=' "$conf"; then
    prev_history="$(config_get "$conf" history_file)"
    prev_history_set=1
  fi
  if grep -qE '^[[:space:]]*tls_cert_file[[:space:]]*=' "$conf"; then
    prev_cert="$(config_get "$conf" tls_cert_file)"
    prev_cert_set=1
  fi
  if grep -qE '^[[:space:]]*tls_key_file[[:space:]]*=' "$conf"; then
    prev_key="$(config_get "$conf" tls_key_file)"
    prev_key_set=1
  fi
  if grep -qE '^[[:space:]]*backup_dir[[:space:]]*=' "$conf"; then
    prev_backup_dir="$(config_get "$conf" backup_dir)"
    prev_backup_dir_set=1
  fi
  if grep -qE '^[[:space:]]*backup_interval_seconds[[:space:]]*=' "$conf"; then
    prev_backup_interval="$(config_get "$conf" backup_interval_seconds)"
    prev_backup_interval_set=1
  fi
  if grep -qE '^[[:space:]]*backup_retention[[:space:]]*=' "$conf"; then
    prev_backup_retention="$(config_get "$conf" backup_retention)"
    prev_backup_retention_set=1
  fi
  if grep -qE '^[[:space:]]*backup_ssh_target[[:space:]]*=' "$conf"; then
    prev_backup_ssh="$(config_get "$conf" backup_ssh_target)"
    prev_backup_ssh_set=1
  fi
  if grep -qE '^[[:space:]]*backup_ssh_port[[:space:]]*=' "$conf"; then
    prev_backup_port="$(config_get "$conf" backup_ssh_port)"
    prev_backup_port_set=1
  fi
  if grep -qE '^[[:space:]]*backup_s3_uri[[:space:]]*=' "$conf"; then
    prev_backup_s3="$(config_get "$conf" backup_s3_uri)"
    prev_backup_s3_set=1
  fi
  if grep -qE '^[[:space:]]*backup_encryption_key_file[[:space:]]*=' "$conf"; then
    prev_backup_key="$(config_get "$conf" backup_encryption_key_file)"
    prev_backup_key_set=1
  fi
  if grep -qE '^[[:space:]]*backup_remote_timeout_seconds[[:space:]]*=' "$conf"; then
    prev_backup_timeout="$(config_get "$conf" backup_remote_timeout_seconds)"
    prev_backup_timeout_set=1
  fi
  if grep -qE '^[[:space:]]*backup_verify_interval_seconds[[:space:]]*=' "$conf"; then
    prev_backup_verify="$(config_get "$conf" backup_verify_interval_seconds)"
    prev_backup_verify_set=1
  fi
  if grep -qE '^[[:space:]]*backup_alert_url[[:space:]]*=' "$conf"; then
    prev_backup_alert="$(config_get "$conf" backup_alert_url)"
    prev_backup_alert_set=1
  fi
  if grep -qE '^[[:space:]]*backup_health_report_url[[:space:]]*=' "$conf"; then
    prev_backup_report="$(config_get "$conf" backup_health_report_url)"
    prev_backup_report_set=1
  fi
  if grep -qE '^[[:space:]]*backup_health_report_interval_seconds[[:space:]]*=' "$conf"; then
    prev_backup_report_interval="$(config_get "$conf" backup_health_report_interval_seconds)"
    prev_backup_report_interval_set=1
  fi

  cur_bind="${cur_bind:-$DEFAULT_BIND}"
  cur_port="${cur_port:-$DEFAULT_PORT}"
  cur_jobs="${cur_jobs:-jobs}"
  cur_logs="${cur_logs:-logs}"
  cur_poll="${cur_poll:-1}"
  cur_timeout="${cur_timeout:-300}"
  # Corrige valores inválidos de configs antigas antes de usá-los como padrão.
  validate_uint "$cur_port" && (( cur_port >= 1 && cur_port <= 65535 )) || cur_port="$DEFAULT_PORT"
  validate_uint "$cur_poll" && (( cur_poll >= 1 && cur_poll <= 3600 )) || cur_poll=1
  validate_uint "$cur_timeout" && (( cur_timeout >= 1 && cur_timeout <= 86400 )) || cur_timeout=300
  validate_uint "$cur_max_jobs" && (( cur_max_jobs >= 1 && cur_max_jobs <= 256 )) || cur_max_jobs=4
  validate_uint "$cur_log_max" || cur_log_max=10485760
  validate_uint "$cur_rate" || cur_rate=600
  validate_uint "$cur_auth_limit" || cur_auth_limit=10
  validate_uint "$cur_auth_window" || cur_auth_window=60
  validate_uint "$cur_auth_lockout" || cur_auth_lockout=300
  validate_uint "$cur_notify" && (( cur_notify >= 1 && cur_notify <= 3600 )) || cur_notify=10
  [[ -n "$cur_jobs" ]] || cur_jobs="jobs"
  [[ -n "$cur_logs" ]] || cur_logs="logs"
  [[ -n "$cur_bind" ]] || cur_bind="$DEFAULT_BIND"
  if is_weak_token "$cur_token"; then cur_token=""; fi

  if [[ "$PROMPT" -eq 1 ]]; then
    printf '\n  Configure a instância (Enter aceita o valor entre colchetes).\n\n' >&2
  fi

  local bind port jobs logs poll timeout token
  bind="$(ask_flag "$OPT_BIND" "Endereço de escuta" "$cur_bind")"
  [[ -n "$bind" ]] || bind="$cur_bind"
  port="$(ask_uint "$OPT_PORT" "Porta do painel" "$cur_port" 1 65535)"
  jobs="$(ask_flag "$OPT_JOBS_DIR" "Diretório de tarefas" "$cur_jobs")"
  logs="$(ask_flag "$OPT_LOGS_DIR" "Diretório de logs" "$cur_logs")"
  poll="$(ask_uint "$OPT_POLL" "Verificação de agenda (segundos)" "$cur_poll" 1 3600)"
  timeout="$(ask_uint "$OPT_TIMEOUT" "Timeout de comando (segundos)" "$cur_timeout" 1 86400)"
  token="$(ask_password "$cur_token")"

  jobs="$(expand_path "$jobs")"
  logs="$(expand_path "$logs")"
  [[ -n "$jobs" && -n "$logs" ]] || die "Os diretórios de tarefas e logs não podem ficar vazios."

  if [[ "$bind" != "127.0.0.1" && "$bind" != "::1" && "$bind" != "localhost" ]]; then
    if [[ "$prev_tls" == "true" ]]; then
      if [[ "$prev_trusted_set" -ne 1 || -z "$prev_trusted" || "$prev_trusted" == "[]" ]]; then
        warn "Defina trusted_proxy_ips com o(s) IP(s) do proxy; sem isso o servidor vai recusar iniciar."
      fi
      if [[ "$prev_trust" != "true" ]]; then
        warn "Considere trust_proxy_headers = true para o rate limit distinguir clientes atrás do proxy."
      fi
    elif [[ -z "$prev_cert" || -z "$prev_key" ]]; then
      warn "Bind em '$bind' sem TLS: o servidor vai recusar iniciar."
      warn "Use 127.0.0.1 atrás de um proxy TLS, configure tls_cert_file/tls_key_file ou use tls_terminated_by_proxy = true."
    fi
  fi

  write_config "$conf" "$bind" "$port" "$jobs" "$logs" "$poll" "$timeout" "$token" \
    "$cur_max_jobs" "$cur_log_max" "$cur_rate" "$cur_auth_limit" "$cur_auth_window" "$cur_auth_lockout" "$cur_notify"
  chmod 600 "$conf" 2>/dev/null || true

  # Preserva opções avançadas que não têm prompt no setup.
  [[ -n "$prev_tls" ]] && config_set_raw "$conf" tls_terminated_by_proxy "$prev_tls"
  [[ -n "$prev_trust" ]] && config_set_raw "$conf" trust_proxy_headers "$prev_trust"
  [[ "$prev_trusted_set" -eq 1 ]] && config_set_raw "$conf" trusted_proxy_ips "$prev_trusted"
  [[ "$prev_audit_set" -eq 1 ]] && config_set_string "$conf" audit_log_file "$prev_audit"
  [[ "$prev_history_set" -eq 1 ]] && config_set_string "$conf" history_file "$prev_history"
  [[ "$prev_cert_set" -eq 1 ]] && config_set_string "$conf" tls_cert_file "$prev_cert"
  [[ "$prev_key_set" -eq 1 ]] && config_set_string "$conf" tls_key_file "$prev_key"
  [[ "$prev_backup_dir_set" -eq 1 ]] && config_set_string "$conf" backup_dir "$prev_backup_dir"
  [[ "$prev_backup_interval_set" -eq 1 ]] && config_set_raw "$conf" backup_interval_seconds "$prev_backup_interval"
  [[ "$prev_backup_retention_set" -eq 1 ]] && config_set_raw "$conf" backup_retention "$prev_backup_retention"
  [[ "$prev_backup_ssh_set" -eq 1 ]] && config_set_string "$conf" backup_ssh_target "$prev_backup_ssh"
  [[ "$prev_backup_port_set" -eq 1 ]] && config_set_raw "$conf" backup_ssh_port "$prev_backup_port"
  [[ "$prev_backup_s3_set" -eq 1 ]] && config_set_string "$conf" backup_s3_uri "$prev_backup_s3"
  [[ "$prev_backup_key_set" -eq 1 ]] && config_set_string "$conf" backup_encryption_key_file "$prev_backup_key"
  [[ "$prev_backup_timeout_set" -eq 1 ]] && config_set_raw "$conf" backup_remote_timeout_seconds "$prev_backup_timeout"
  [[ "$prev_backup_verify_set" -eq 1 ]] && config_set_raw "$conf" backup_verify_interval_seconds "$prev_backup_verify"
  [[ "$prev_backup_alert_set" -eq 1 ]] && config_set_string "$conf" backup_alert_url "$prev_backup_alert"
  [[ "$prev_backup_report_set" -eq 1 ]] && config_set_string "$conf" backup_health_report_url "$prev_backup_report"
  [[ "$prev_backup_report_interval_set" -eq 1 ]] && config_set_raw "$conf" backup_health_report_interval_seconds "$prev_backup_report_interval"

  # Preserva o modo mais seguro (segredo em arquivo separado) ao reconfigurar.
  if [[ -n "$cur_token_file" ]]; then
    [[ "$cur_token_file" = /* ]] || cur_token_file="$(cd "$(dirname "$conf")" && pwd)/${cur_token_file}"
    local old_umask; old_umask="$(umask)"
    umask 077
    printf '%s\n' "$token" > "$cur_token_file"
    umask "$old_umask"
    chmod 600 "$cur_token_file"
    config_set_string "$conf" api_token_file "$cur_token_file"
    config_remove "$conf" api_token
  fi

  local base jobs_abs logs_abs
  base="$(cd "$(dirname "$conf")" && pwd)"
  jobs_abs="$jobs"; [[ "$jobs_abs" = /* ]] || jobs_abs="${base}/${jobs_abs}"
  logs_abs="$logs"; [[ "$logs_abs" = /* ]] || logs_abs="${base}/${logs_abs}"
  mkdir -p "$jobs_abs" "$logs_abs"

  PROMPT="$old_prompt"
  build_release

  local url; url="$(panel_url "$conf")"
  ok "Configuração salva em $conf"
  printf '\n  Painel:   %s\n  Token:    %s…%s\n  Tarefas:  %s\n  Logs:     %s\n' \
    "$url" "${token:0:4}" "${token: -4}" "$jobs_abs" "$logs_abs"
  printf '  Iniciar:  %s run\n  Serviço:  sudo %s install\n\n' "$SCRIPT_PATH" "$SCRIPT_PATH"

  if [[ -f "$UNIT_FILE" ]]; then
    if [[ "$OPT_YES" -eq 0 && -t 0 ]] && confirm "Serviço systemd instalado detectado. Sincronizar a nova config e reiniciar agora?"; then
      "$SCRIPT_PATH" install --no-build
    else
      warn "O serviço ainda usa ${CONF_FILE}. Sincronize com: sudo ${SCRIPT_PATH} install"
    fi
  fi
}

cmd_build() {
  build_release
}

cmd_run() {
  ensure_local_config "$LOCAL_CONFIG"
  build_release
  local url; url="$(panel_url "$LOCAL_CONFIG")"
  ok "Backend + frontend em ${C_BOLD}${url}${C_RESET} (Ctrl+C para encerrar)"
  exec "$ROOT_DIR/target/release/datatask" "$LOCAL_CONFIG"
}

cmd_dev() {
  ensure_cargo
  ensure_local_config "$LOCAL_CONFIG"
  local url; url="$(panel_url "$LOCAL_CONFIG")"
  ok "Modo desenvolvimento em ${C_BOLD}${url}${C_RESET} (Ctrl+C para encerrar)"
  exec cargo run --manifest-path "$ROOT_DIR/Cargo.toml" -- "$LOCAL_CONFIG"
}

# Escolhe qual config token/url devem usar: a do serviço (se instalado) ou a local.
resolve_target_config() {
  if [[ "$OPT_LOCAL" -eq 1 ]]; then
    printf '%s' "$LOCAL_CONFIG"
    return 0
  fi
  if [[ "$OPT_SERVICE" -eq 1 ]]; then
    printf '%s' "$CONF_FILE"
    return 0
  fi
  if [[ -f "$UNIT_FILE" ]]; then
    printf '%s' "$CONF_FILE"
    return 0
  fi
  printf '%s' "$LOCAL_CONFIG"
}

cmd_token() {
  local conf; conf="$(resolve_target_config)"
  [[ -f "$conf" ]] || die "Config não encontrado: $conf (rode '${SCRIPT_PATH} setup' ou 'sudo ${SCRIPT_PATH} install')"
  [[ -r "$conf" ]] || need_root
  local token; token="$(config_token "$conf")"
  [[ -n "$token" ]] || die "api_token/api_token_file ausente em $conf"
  printf 'Token de %s:\n' "$conf" >&2
  printf '%s\n' "$token"
}

cmd_url() {
  local conf; conf="$(resolve_target_config)"
  [[ -f "$conf" ]] || die "Config não encontrado: $conf"
  [[ -r "$conf" ]] || need_root
  printf 'URL de %s:\n' "$conf" >&2
  panel_url "$conf"; printf '\n'
}

http_status_auth() {
  local endpoint="$1" token="$2" insecure=()
  if command -v curl >/dev/null 2>&1; then
    [[ "$endpoint" == https://* ]] && insecure=(-k)
    # Pass the credential on stdin (`-H @-`) so it never appears in `ps`.
    printf 'Authorization: Bearer %s\n' "$token" \
      | curl -s "${insecure[@]}" -o /dev/null -w '%{http_code}' --max-time 3 \
          -H @- "${endpoint}/api/jobs" 2>/dev/null || true
  else
    echo "000"
  fi
}

mask_token() {
  local token="$1"
  if [[ "${#token}" -le 8 ]]; then printf '%s' "$token"
  else printf '%s…%s' "${token:0:6}" "${token: -4}"
  fi
}

# Diagnostica divergências entre a config do projeto e a do serviço.
cmd_doctor() {
  local proj_conf="$LOCAL_CONFIG" serv_conf="$CONF_FILE"
  local proj_tok="" serv_tok="" proj_url="" serv_url=""
  local serv_state="não instalado"

  printf '\n%sDataTask · diagnóstico%s\n\n' "$C_BOLD" "$C_RESET"

  if [[ -f "$serv_conf" && ! -r "$serv_conf" ]]; then
    need_root
  fi

  if [[ -f "$proj_conf" ]]; then
    proj_url="$(panel_url "$proj_conf")"
    proj_tok="$(config_token "$proj_conf")"
    printf '  %sProjeto:%s %s\n    URL: %s · token: %s\n' \
      "$C_BOLD" "$C_RESET" "$proj_conf" "${proj_url:-?}" "$(mask_token "$proj_tok")"
  else
    printf '  %sProjeto:%s %s (ausente)\n' "$C_BOLD" "$C_RESET" "$proj_conf"
  fi

  if [[ -f "$serv_conf" ]]; then
    serv_url="$(panel_url "$serv_conf")"
    serv_tok="$(config_token "$serv_conf")"
    printf '  %sServiço:%s %s\n    URL: %s · token: %s\n' \
      "$C_BOLD" "$C_RESET" "$serv_conf" "${serv_url:-?}" "$(mask_token "$serv_tok")"
  else
    printf '  %sServiço:%s %s (não instalado)\n' "$C_BOLD" "$C_RESET" "$serv_conf"
  fi

  if [[ -f "$UNIT_FILE" ]]; then
    if command -v systemctl >/dev/null 2>&1; then
      serv_state="$(systemctl is-active "$SERVICE_NAME" 2>/dev/null || true)"
      serv_state="${serv_state:-desconhecido}"
    fi
    printf '  %sSystemd:%s %s (%s)\n' "$C_BOLD" "$C_RESET" "$serv_state" "$UNIT_FILE"
  fi

  if [[ -n "$proj_tok" && -n "$serv_tok" ]]; then
    if [[ "$proj_tok" == "$serv_tok" ]]; then
      ok "Tokens do projeto e do serviço são iguais."
    else
      warn "Tokens DIFERENTES: use o token do serviço ou sincronize com 'sudo ${SCRIPT_PATH} install'."
    fi
  fi

  printf '\n  %sSaúde e autenticação:%s\n' "$C_BOLD" "$C_RESET"
  local url token label status
  for pair in "${proj_url}|${proj_tok}|projeto" "${serv_url}|${serv_tok}|serviço"; do
    IFS='|' read -r url token label <<< "$pair"
    [[ -n "$url" ]] || continue
    if http_get "${url}/health" >/dev/null 2>&1; then
      printf '    %-9s %s · health: %sok%s' "$label" "$url" "$C_GREEN" "$C_RESET"
      if [[ -n "$token" ]]; then
        status="$(http_status_auth "$url" "$token")"
        if [[ "$status" == "200" ]]; then
          printf ' · token: %sok%s\n' "$C_GREEN" "$C_RESET"
        elif [[ "$status" == "401" ]]; then
          printf ' · token: %s401 (inválido para este serviço)%s\n' "$C_RED" "$C_RESET"
        else
          printf ' · token: HTTP %s\n' "$status"
        fi
      else
        printf '\n'
      fi
    else
      printf '    %-9s %s · %ssem resposta%s\n' "$label" "$url" "$C_YELLOW" "$C_RESET"
    fi
  done

  printf '\n'
  if [[ -f "$UNIT_FILE" && -n "$serv_tok" && "$proj_tok" != "$serv_tok" ]]; then
    printf '  %sCorreção:%s sudo %s install          # sincroniza a config do projeto no serviço\n' "$C_BOLD" "$C_RESET" "$SCRIPT_PATH"
    printf '  %sOu:%s      sudo %s token --service   # usa o token atual do serviço no painel\n\n' "$C_BOLD" "$C_RESET" "$SCRIPT_PATH"
  elif [[ "$serv_state" != "active" && -f "$UNIT_FILE" ]]; then
    printf '  %sInicie o serviço:%s sudo %s start\n\n' "$C_BOLD" "$C_RESET" "$SCRIPT_PATH"
  else
    printf '  %sNenhuma divergência encontrada.%s\n\n' "$C_GREEN" "$C_RESET"
  fi
}

# ----------------------------------------------------------- serviço systemd --
resolve_service_user() {
  local dry="${1:-0}"
  if [[ -n "$OPT_USER" ]]; then
    SERVICE_USER="$OPT_USER"
  elif [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
    SERVICE_USER="$SUDO_USER"
  else
    local owner; owner="$(stat -c '%U' "$ROOT_DIR")"
    if [[ "$owner" == "root" ]]; then
      SERVICE_USER="datatask"
    else
      SERVICE_USER="$owner"
    fi
  fi

  if [[ "$SERVICE_USER" == "root" ]]; then
    warn "Executar os jobs como root é perigoso. Use --user <usuário>."
  fi

  if id "$SERVICE_USER" >/dev/null 2>&1; then
    SERVICE_GROUP="$(id -gn "$SERVICE_USER")"
    return 0
  fi

  if [[ "$SERVICE_USER" == "datatask" ]]; then
    if [[ "$dry" -eq 1 ]]; then
      SERVICE_GROUP="datatask"
      return 0
    fi
    log "Criando usuário de sistema 'datatask'…"
    useradd --system --home-dir "$DATA_DIR" --shell /usr/sbin/nologin "$SERVICE_USER"
    SERVICE_GROUP="$(id -gn "$SERVICE_USER")"
    ok "Usuário 'datatask' criado."
    return 0
  fi

  die "Usuário '$SERVICE_USER' não existe. Crie-o ou use --user <usuário>."
}

render_unit() {
  cat <<EOF
[Unit]
Description=DataTask — backend + frontend (painel web) do agendador de tarefas
Documentation=file://${ROOT_DIR}/README.md
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${SERVICE_USER}
Group=${SERVICE_GROUP}
WorkingDirectory=${DATA_DIR}
ExecStart=${BIN_PATH} ${CONF_FILE}
ExecReload=/bin/kill -HUP \$MAINPID
Restart=on-failure
RestartSec=3
TimeoutStopSec=10
Environment=RUST_LOG=info

# Endurecimento básico. Os jobs ainda executam comandos arbitrários definidos
# pelo usuário; mantenha o token da API em segredo.
NoNewPrivileges=yes
ProtectSystem=full
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
RestrictSUIDSGID=yes
RestrictNamespaces=yes
RestrictRealtime=yes
LockPersonality=yes
CapabilityBoundingSet=
SystemCallArchitectures=native
UMask=0077
LimitCORE=0

[Install]
WantedBy=multi-user.target
EOF
}

wait_health() {
  local conf="$1" url tries=30
  url="$(panel_url "$conf")"
  for ((attempt = 1; attempt <= tries; attempt++)); do
    if http_get "${url}/health" >/dev/null 2>&1 && http_get "${url}/readyz" >/dev/null 2>&1; then
      ok "Serviço respondendo em ${url} (health + readyz)"
      return 0
    fi
    sleep 1
  done
  warn "O serviço não respondeu em ${tries}s. Verifique: sudo journalctl -u ${SERVICE_NAME} -n 50 --no-pager"
  return 1
}

# Confirma que o mesmo serviço também está entregando o painel (frontend).
verify_ui() {
  local conf="$1" base failed=0
  base="$(panel_url "$conf")"
  if http_get "${base}/" 2>/dev/null | grep -q "DataTask"; then
    ok "Frontend: / (painel) OK"
  else
    warn "Frontend: / não respondeu como esperado."
    failed=1
  fi
  local asset
  for asset in app.css app.js; do
    if http_get "${base}/${asset}" >/dev/null 2>&1; then
      ok "Frontend: /${asset} OK"
    else
      warn "Frontend: /${asset} não encontrado."
      failed=1
    fi
  done
  if [[ "$failed" -eq 0 ]]; then
    ok "Backend + frontend ativos no mesmo serviço (${base})."
  fi
  return 0
}

prepare_service_files() {
  local source="$1" bind="$2" port="$3"

  install -d -m 755 "$CONF_DIR"
  if [[ -f "$source" && "$(readlink -f "$source")" != "$(readlink -f "$CONF_FILE" 2>/dev/null || echo "")" ]]; then
    if [[ -f "$CONF_FILE" && "$OPT_YES" -eq 0 ]]; then
      warn "Config existente será substituída por $source (backup em ${CONF_FILE}.bak)."
    fi
    [[ -f "$CONF_FILE" ]] && cp -f "$CONF_FILE" "${CONF_FILE}.bak" && chmod 600 "${CONF_FILE}.bak"
    cp -f "$source" "$CONF_FILE"
    ok "Config copiada de $source"
  fi

  if [[ ! -f "$CONF_FILE" ]]; then
    write_config "$CONF_FILE" "$bind" "$port" "$JOBS_DIR" "$LOGS_DIR"
    ok "Configuração criada em $CONF_FILE"
  else
    [[ -n "$OPT_BIND" ]] && config_set_string "$CONF_FILE" bind_address "$OPT_BIND"
    [[ -n "$OPT_PORT" ]] && config_set_number "$CONF_FILE" port "$OPT_PORT"
  fi

  # api_token_file relativo é relativo à config; ancora em /etc/datatask.
  local current_token_file; current_token_file="$(config_get "$CONF_FILE" api_token_file)"
  if [[ -n "$current_token_file" && "$current_token_file" != /* ]]; then
    config_set_string "$CONF_FILE" api_token_file "${CONF_DIR}/${current_token_file}"
  fi
  ensure_token "$CONF_FILE"

  # Mantém o segredo do serviço em arquivo próprio (600) em vez da config.
  if [[ -z "$(config_get "$CONF_FILE" api_token_file)" ]]; then
    local service_token; service_token="$(config_get "$CONF_FILE" api_token)"
    if [[ -n "$service_token" ]]; then
      local token_file="${CONF_DIR}/api_token" old_umask
      old_umask="$(umask)"
      umask 077
      printf '%s\n' "$service_token" > "$token_file"
      umask "$old_umask"
      chmod 600 "$token_file"
      config_set_string "$CONF_FILE" api_token_file "$token_file"
      config_remove "$CONF_FILE" api_token
      ok "Token do serviço movido para $token_file (modo 600)"
    fi
  fi

  # Caminhos relativos de TLS são resolvidos a partir da config de origem.
  local cert_base cert_file key_file
  if [[ -n "$source" && -f "$source" ]]; then
    cert_base="$(cd "$(dirname "$source")" && pwd)"
  else
    cert_base="$CONF_DIR"
  fi
  cert_file="$(config_get "$CONF_FILE" tls_cert_file)"
  key_file="$(config_get "$CONF_FILE" tls_key_file)"
  if [[ -n "$cert_file" && "$cert_file" != /* ]]; then
    config_set_string "$CONF_FILE" tls_cert_file "${cert_base}/${cert_file}"
  fi
  if [[ -n "$key_file" && "$key_file" != /* ]]; then
    config_set_string "$CONF_FILE" tls_key_file "${cert_base}/${key_file}"
  fi
  local backup_key; backup_key="$(config_get "$CONF_FILE" backup_encryption_key_file)"
  if [[ -n "$backup_key" && "$backup_key" != /* ]]; then
    config_set_string "$CONF_FILE" backup_encryption_key_file "${cert_base}/${backup_key}"
  fi

  # Se a config veio de outro lugar com caminhos relativos, ancora em DATA_DIR.
  local jobs logs
  jobs="$(config_get "$CONF_FILE" jobs_dir)"
  logs="$(config_get "$CONF_FILE" logs_dir)"
  if [[ -n "$jobs" && "$jobs" != /* ]]; then
    config_set_string "$CONF_FILE" jobs_dir "$JOBS_DIR"
    jobs="$JOBS_DIR"
  fi
  if [[ -n "$logs" && "$logs" != /* ]]; then
    config_set_string "$CONF_FILE" logs_dir "$LOGS_DIR"
    logs="$LOGS_DIR"
  fi
  jobs="${jobs:-$JOBS_DIR}"
  logs="${logs:-$LOGS_DIR}"

  local backup; backup="$(config_get "$CONF_FILE" backup_dir)"
  if [[ -n "$backup" && "$backup" != /* ]]; then
    config_set_string "$CONF_FILE" backup_dir "${DATA_DIR}/${backup}"
  fi

  local bind_addr tls_proxy trusted_raw native_cert native_key
  bind_addr="$(config_get "$CONF_FILE" bind_address)"
  tls_proxy="$(config_get "$CONF_FILE" tls_terminated_by_proxy)"
  trusted_raw="$(config_get_raw "$CONF_FILE" trusted_proxy_ips)"
  native_cert="$(config_get "$CONF_FILE" tls_cert_file)"
  native_key="$(config_get "$CONF_FILE" tls_key_file)"
  if [[ -n "$native_cert" || -n "$native_key" ]]; then
    if [[ -z "$native_cert" || -z "$native_key" ]]; then
      die "Configure tls_cert_file e tls_key_file juntos em $CONF_FILE."
    fi
    [[ -f "$native_cert" ]] || die "TLS cert não encontrado: $native_cert"
    [[ -f "$native_key" ]] || die "TLS key não encontrado: $native_key"
    local key_mode; key_mode="$(stat -c '%a' "$native_key" 2>/dev/null || echo 600)"
    if (( 8#$key_mode & 077 )); then
      warn "A chave TLS $native_key está acessível por grupo/outros; use chmod 600."
    fi
  fi
  if [[ -n "$native_cert" && -n "$native_key" && "$tls_proxy" == "true" ]]; then
    die "Escolha TLS nativo (tls_cert_file/tls_key_file) ou tls_terminated_by_proxy, não ambos, em $CONF_FILE."
  fi
  if [[ "$bind_addr" != "127.0.0.1" && "$bind_addr" != "::1" && "$bind_addr" != "localhost" && "$tls_proxy" != "true" ]]; then
    if [[ -z "$native_cert" || -z "$native_key" ]]; then
      die "bind_address='${bind_addr}' exige TLS. Use 127.0.0.1 atrás de um proxy TLS, configure tls_cert_file/tls_key_file ou defina tls_terminated_by_proxy = true em $CONF_FILE."
    fi
  fi
  if [[ "$bind_addr" != "127.0.0.1" && "$bind_addr" != "::1" && "$bind_addr" != "localhost" && "$tls_proxy" == "true" ]]; then
    if [[ -z "$trusted_raw" || "$trusted_raw" == "[]" ]]; then
      die "bind_address='${bind_addr}' com tls_terminated_by_proxy exige trusted_proxy_ips = [\"<ip-do-proxy>\"] em $CONF_FILE."
    fi
  fi

  install -d -m 700 "$DATA_DIR" "$jobs" "$logs"
  chown -R "${SERVICE_USER}:${SERVICE_GROUP}" "$DATA_DIR"
  chown "${SERVICE_USER}:${SERVICE_GROUP}" "$CONF_FILE"
  chmod 600 "$CONF_FILE"
  chmod 755 "$CONF_DIR"
  # Tarefas e logs podem conter segredos: nada de leitura por grupo/outros.
  chmod -R u+rwX,go-rwx "$DATA_DIR" 2>/dev/null || true

  local svc_token_file; svc_token_file="$(config_get "$CONF_FILE" api_token_file)"
  if [[ -n "$svc_token_file" ]]; then
    [[ "$svc_token_file" = /* ]] || svc_token_file="${CONF_DIR}/${svc_token_file}"
    if [[ -f "$svc_token_file" ]]; then
      if [[ "$svc_token_file" == "$CONF_DIR/"* || "$svc_token_file" == "$DATA_DIR/"* ]]; then
        chown "${SERVICE_USER}:${SERVICE_GROUP}" "$svc_token_file"
        chmod 600 "$svc_token_file"
      else
        warn "Token em $svc_token_file está fora de $CONF_DIR/$DATA_DIR; garanta modo 600 e leitura por $SERVICE_USER."
      fi
    fi
  fi
}

install_root() {
  local source=""
  if [[ -n "$OPT_CONFIG" ]]; then
    source="$LOCAL_CONFIG"
  elif [[ "$OPT_KEEP_CONFIG" -eq 0 && -f "$LOCAL_CONFIG" ]]; then
    # Mantém uma única fonte de verdade: a config do projeto (mesma senha/porta).
    source="$LOCAL_CONFIG"
  fi
  local bind="${OPT_BIND:-$DEFAULT_BIND}" port="${OPT_PORT:-$DEFAULT_PORT}"

  if [[ -n "$source" && ! -f "$source" ]]; then
    die "Config de origem não encontrada: $source"
  fi

  # A config de destino define porta/bind quando não passados.
  if [[ -z "$OPT_PORT" && -f "$CONF_FILE" ]]; then
    port="$(config_get "$CONF_FILE" port)"
    port="${port:-$DEFAULT_PORT}"
  elif [[ -z "$OPT_PORT" && -f "$LOCAL_CONFIG" ]]; then
    port="$(config_get "$LOCAL_CONFIG" port)"
    port="${port:-$DEFAULT_PORT}"
  fi
  if [[ -z "$OPT_BIND" && -f "$CONF_FILE" ]]; then
    bind="$(config_get "$CONF_FILE" bind_address)"
    bind="${bind:-$DEFAULT_BIND}"
  elif [[ -z "$OPT_BIND" && -f "$LOCAL_CONFIG" ]]; then
    bind="$(config_get "$LOCAL_CONFIG" bind_address)"
    bind="${bind:-$DEFAULT_BIND}"
  fi

  log "Instalando o serviço ${SERVICE_NAME}…"
  prepare_service_files "$source" "$bind" "$port"

  if [[ "$OPT_REBUILD" -eq 1 || "$OPT_NO_BUILD" -eq 0 && ! -x "$ROOT_DIR/target/release/datatask" ]]; then
    build_release
  fi
  [[ -x "$ROOT_DIR/target/release/datatask" ]] \
    || die "Binário não encontrado. Rode '${SCRIPT_PATH} build' e tente novamente."

  install -m 755 "$ROOT_DIR/target/release/datatask" "$BIN_PATH"
  ok "Binário instalado em $BIN_PATH"

  render_unit > "$UNIT_FILE"
  chmod 644 "$UNIT_FILE"
  ok "Unit file criado em $UNIT_FILE"

  systemctl daemon-reload
  systemctl enable "$SERVICE_NAME" >/dev/null
  systemctl restart "$SERVICE_NAME"
  ok "Serviço habilitado e iniciado."

  wait_health "$CONF_FILE" || true
  verify_ui "$CONF_FILE"

  local url; url="$(panel_url "$CONF_FILE")"
  printf '\n  %sPainel:%s     %s\n' "$C_BOLD" "$C_RESET" "$url"
  printf '  %sConfig:%s     %s\n' "$C_BOLD" "$C_RESET" "$CONF_FILE"
  printf '  %sDados:%s      %s\n' "$C_BOLD" "$C_RESET" "$DATA_DIR"
  printf '  %sToken:%s      sudo %s token --service\n' "$C_BOLD" "$C_RESET" "$SCRIPT_PATH"
  printf '  %sLogs:%s       sudo %s logs\n\n' "$C_BOLD" "$C_RESET" "$SCRIPT_PATH"
}

cmd_install() {
  if [[ "$OPT_DRY_RUN" -eq 1 ]]; then
    resolve_service_user 1
    log "Simulação (dry-run) do install:"
    printf '\n  Serviço: backend + frontend (painel embutido, mesmo processo)\n'
    printf '  Usuário do serviço: %s:%s\n  Unit file: %s\n' "$SERVICE_USER" "$SERVICE_GROUP" "$UNIT_FILE"
    printf '  Binário: %s\n  Config: %s\n  Dados: %s\n\n' "$BIN_PATH" "$CONF_FILE" "$DATA_DIR"
    render_unit
    return 0
  fi

  if [[ "$EUID" -ne 0 ]]; then
    # Compila como o usuário atual antes de elevar privilégios.
    ensure_local_config "$LOCAL_CONFIG"
    build_release
    warn "Precisa de sudo para instalar o serviço; elevando privilégios…"
    exec sudo -- "$SCRIPT_PATH" "${ORIG_ARGS[@]}"
  fi

  need_root
  require_systemd
  resolve_service_user
  install_root
}

cmd_uninstall() {
  need_root
  require_systemd
  log "Removendo o serviço ${SERVICE_NAME}…"
  if systemctl list-unit-files "${SERVICE_NAME}.service" >/dev/null 2>&1; then
    systemctl stop "$SERVICE_NAME" 2>/dev/null || true
    systemctl disable "$SERVICE_NAME" 2>/dev/null || true
  fi
  rm -f "$UNIT_FILE"
  systemctl daemon-reload
  systemctl reset-failed "$SERVICE_NAME" 2>/dev/null || true
  rm -f "$BIN_PATH"
  ok "Serviço removido do systemd."

  if [[ "$OPT_PURGE" -eq 1 ]]; then
    if confirm "Apagar também config, jobs e logs em ${CONF_DIR} e ${DATA_DIR}?"; then
      rm -rf "$CONF_DIR" "$DATA_DIR"
      ok "Config e dados removidos."
    else
      warn "Dados mantidos em ${CONF_DIR} e ${DATA_DIR}."
    fi
  else
    printf '  Config mantida: %s\n  Dados mantidos: %s\n  Para apagar tudo: sudo %s uninstall --purge\n\n' \
      "$CONF_DIR" "$DATA_DIR" "$SCRIPT_PATH"
  fi
}

service_action() {
  local action="$1"
  need_root
  require_systemd
  systemctl "$action" "$SERVICE_NAME"
}

cmd_status() {
  if ! command -v systemctl >/dev/null 2>&1; then
    die "systemctl não encontrado."
  fi
  systemctl status "$SERVICE_NAME" --no-pager || true
  local url; url="$(panel_url "$CONF_FILE")"
  printf '\n  Painel: %s\n' "$url"
  if http_get "${url}/health" >/dev/null 2>&1; then
    ok "Health check OK"
    if http_get "${url}/readyz" >/dev/null 2>&1; then
      ok "Readiness check OK"
    else
      warn "Readiness indisponível (scheduler ainda iniciando ou parado)"
    fi
    if http_get "${url}/metrics" >/dev/null 2>&1; then
      ok "Metrics check OK"
    else
      warn "Metrics indisponível"
    fi
    verify_ui "$CONF_FILE"
  else
    warn "Health check falhou"
  fi
}

cmd_logs() {
  need_root
  require_systemd
  if [[ "$OPT_FOLLOW" -eq 1 ]]; then
    journalctl -u "$SERVICE_NAME" -n 200 -f
  else
    journalctl -u "$SERVICE_NAME" -n 200 --no-pager
  fi
}

# ------------------------------------------------------------------- parsing --
while [[ $# -gt 0 ]]; do
  case "$1" in
    --config) [[ $# -ge 2 ]] || die "--config exige um valor."; OPT_CONFIG="$2"; shift 2 ;;
    --config=*) OPT_CONFIG="${1#*=}"; shift ;;
    --port) [[ $# -ge 2 ]] || die "--port exige um valor."; OPT_PORT="$2"; shift 2 ;;
    --port=*) OPT_PORT="${1#*=}"; shift ;;
    --bind) [[ $# -ge 2 ]] || die "--bind exige um valor."; OPT_BIND="$2"; shift 2 ;;
    --bind=*) OPT_BIND="${1#*=}"; shift ;;
    --user) [[ $# -ge 2 ]] || die "--user exige um valor."; OPT_USER="$2"; shift 2 ;;
    --user=*) OPT_USER="${1#*=}"; shift ;;
    --jobs-dir) [[ $# -ge 2 ]] || die "--jobs-dir exige um valor."; OPT_JOBS_DIR="$2"; shift 2 ;;
    --jobs-dir=*) OPT_JOBS_DIR="${1#*=}"; shift ;;
    --logs-dir) [[ $# -ge 2 ]] || die "--logs-dir exige um valor."; OPT_LOGS_DIR="$2"; shift 2 ;;
    --logs-dir=*) OPT_LOGS_DIR="${1#*=}"; shift ;;
    --poll) [[ $# -ge 2 ]] || die "--poll exige um valor."; OPT_POLL="$2"; shift 2 ;;
    --poll=*) OPT_POLL="${1#*=}"; shift ;;
    --timeout) [[ $# -ge 2 ]] || die "--timeout exige um valor."; OPT_TIMEOUT="$2"; shift 2 ;;
    --timeout=*) OPT_TIMEOUT="${1#*=}"; shift ;;
    --purge) OPT_PURGE=1; shift ;;
    --yes|-y) OPT_YES=1; shift ;;
    --rebuild) OPT_REBUILD=1; shift ;;
    --no-build) OPT_NO_BUILD=1; shift ;;
    --with-deps) OPT_DEPS=1; shift ;;
    --dry-run) OPT_DRY_RUN=1; shift ;;
    --keep-config) OPT_KEEP_CONFIG=1; shift ;;
    --service) OPT_SERVICE=1; shift ;;
    --local) OPT_LOCAL=1; shift ;;
    --no-follow) OPT_FOLLOW=0; shift ;;
    -h|--help|help) usage; exit 0 ;;
    --) shift; break ;;
    -*) die "Opção desconhecida: $1 (use --help)" ;;
    *)
      if [[ -z "$CMD" ]]; then CMD="$1"; shift
      else die "Argumento inesperado: $1"
      fi
      ;;
  esac
done

CMD="${CMD:-help}"
resolve_options

case "$CMD" in
  setup) cmd_setup ;;
  build) cmd_build ;;
  run|serve) cmd_run ;;
  dev) cmd_dev ;;
  token) cmd_token ;;
  url) cmd_url ;;
  doctor) cmd_doctor ;;
  install) cmd_install ;;
  uninstall) cmd_uninstall ;;
  start|stop|restart|reload) service_action "$CMD" ;;
  status) cmd_status ;;
  logs) cmd_logs ;;
  help) usage ;;
  *) die "Comando desconhecido: $CMD (use --help)" ;;
esac
