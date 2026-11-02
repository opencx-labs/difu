#!/usr/bin/env bash
# Server setup for `difu server add` and `difu server upgrade`.
#
# difu runs this as root over SSH (`bash -s`, script on stdin) after replacing
# the parameter line below with shell-quoted values:
#
#   DIFU_STEPS       steps to run, in order (see `main`); `difu server upgrade`
#                    runs `difu tools image service`
#   DIFU_VERSION     difu release to install, e.g. 0.36.0
#   DIFU_TARGET      release target, e.g. x86_64-unknown-linux-musl
#   DIFU_PUBLIC_KEY  this computer's ed25519 key for the server (keys step)
#   DIFU_PORT        difu's port for clients (3438)
#   DIFU_SSH_PORT    the SSH port difu connected to
#   DIFU_TAILSCALE   1 to join Tailscale
#   DIFU_SANDBOX     1 to run every session in a rootless Podman sandbox
#   DIFU_UNIT        difu-agent.service, with @UID@ and @HOME@ placeholders
#
# Debian and Ubuntu only. Every step is safe to repeat.
set -Eeuo pipefail
# @PARAMETERS@

export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export DEBIAN_FRONTEND=noninteractive
readonly LOG=/var/log/difu-setup.log
WORK=

say() { printf '      %s\n' "$*"; }

fail() {
  printf '      Error: %s\n' "$*" >&2
  exit 1
}

# Runs a noisy command with its output in the log; shows the end of the log
# when it fails.
quiet() {
  if ! "$@" >>"$LOG" 2>&1; then
    printf '      Error: `%s` failed. The end of %s:\n' "$*" "$LOG" >&2
    tail -n 25 "$LOG" | sed 's/^/        /' >&2
    exit 1
  fi
}

# Like `quiet`, but reports failure to the caller instead of stopping.
try() {
  "$@" >>"$LOG" 2>&1
}

apt_get() {
  quiet apt-get -y -q -o DPkg::Lock::Timeout=600 \
    -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$@"
}

difu_home() { getent passwd difu | cut -d: -f6; }

# Runs a command as the difu user with the environment its service gets.
as_difu() {
  local uid home
  uid=$(id -u difu)
  home=$(difu_home)
  (cd "$home" && runuser -u difu -- env -i HOME="$home" USER=difu LOGNAME=difu \
    PATH="$home/.local/bin:/usr/local/bin:/usr/bin:/bin" \
    XDG_RUNTIME_DIR="/run/user/$uid" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus" "$@")
}

# Runs Claude Code as the difu user. Once difu moved its config into
# ~/.claude (~/.claude.json is then a link), it shares the service's
# CLAUDE_CONFIG_DIR and so its lock; before that, ~/.claude.json is the file.
as_claude() {
  local home
  home=$(difu_home)
  if [ -L "$home/.claude.json" ]; then
    as_difu env CLAUDE_CONFIG_DIR="$home/.claude" "$@"
  else
    as_difu "$@"
  fi
}

require_debian() {
  [ "$(id -u)" = 0 ] || fail "The setup must run as root."
  local id='' like='' pretty=''
  if [ -r /etc/os-release ]; then
    id=$(. /etc/os-release && printf '%s' "${ID:-}")
    like=$(. /etc/os-release && printf '%s' "${ID_LIKE:-}")
    pretty=$(. /etc/os-release && printf '%s' "${PRETTY_NAME:-}")
  fi
  case " $id $like " in
    *" debian "* | *" ubuntu "*) ;;
    *) fail "difu sets up Debian and Ubuntu servers; this one runs ${pretty:-an unknown system}. Reinstall it with Ubuntu 24.04 LTS or Debian 12 and run difu server add again." ;;
  esac
  command -v apt-get >/dev/null || fail "apt-get is missing; difu needs a Debian or Ubuntu server."
  command -v systemctl >/dev/null || fail "systemd is missing; difu runs as a systemd service."
}

# Adds the key once, keeping the file's other keys intact.
authorize() {
  local user=$1 home=$2 group file
  group=$(id -gn "$user")
  install -d -m 700 -o "$user" -g "$group" "$home/.ssh"
  file="$home/.ssh/authorized_keys"
  touch "$file"
  if ! grep -qxF "$DIFU_PUBLIC_KEY" "$file"; then
    if [ -s "$file" ] && [ -n "$(tail -c 1 "$file")" ]; then
      printf '\n' >>"$file"
    fi
    printf '%s\n' "$DIFU_PUBLIC_KEY" >>"$file"
  fi
  chown "$user:$group" "$file"
  chmod 600 "$file"
}

step_keys() {
  say "Creating the difu user and installing this computer's key"
  case "$DIFU_PUBLIC_KEY" in
    ssh-ed25519\ *) ;;
    *) fail "Missing this computer's public key." ;;
  esac
  if ! id difu >/dev/null 2>&1; then
    quiet useradd --create-home --user-group --shell /bin/bash difu
  fi
  # `*` allows no password but, unlike a locked `!`, still allows key logins.
  quiet usermod --password '*' difu
  local group
  for group in sudo admin wheel; do
    if id -nG difu | tr ' ' '\n' | grep -qx "$group"; then
      quiet gpasswd -d difu "$group"
    fi
  done
  authorize root /root
  authorize difu "$(difu_home)"
}

step_ssh() {
  say "Turning off SSH password logins"
  local conf=/etc/ssh/sshd_config.d/10-difu.conf
  install -d -m 755 /etc/ssh/sshd_config.d
  if ! grep -qiE '^[[:space:]]*Include[[:space:]]+/etc/ssh/sshd_config\.d/' /etc/ssh/sshd_config; then
    sed -i '1i Include /etc/ssh/sshd_config.d/*.conf' /etc/ssh/sshd_config
  fi
  cat >"$conf" <<'EOF'
# Written by `difu server add`: key-only SSH.
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin prohibit-password
EOF
  chmod 644 "$conf"
  mkdir -p /run/sshd
  if ! sshd -t >>"$LOG" 2>&1; then
    rm -f "$conf"
    fail "The SSH server rejected difu's settings (details in $LOG); password logins stay on. Fix /etc/ssh/sshd_config and run difu server add again."
  fi
  if systemctl is-active --quiet ssh; then
    quiet systemctl reload ssh
  elif systemctl is-active --quiet sshd; then
    quiet systemctl reload sshd
  fi
  if ! sshd -T 2>/dev/null | grep -qx 'passwordauthentication no'; then
    say "Warning: another SSH setting keeps password logins on; check /etc/ssh/sshd_config."
  fi
}

step_packages() {
  say "Installing git, gh, podman and system tools (a few minutes)"
  if command -v cloud-init >/dev/null; then
    # A freshly booted cloud server may still be installing its own updates.
    cloud-init status --wait >/dev/null 2>&1 || true
  fi
  apt_get update
  apt_get install curl ca-certificates
  install -d -m 755 /etc/apt/keyrings
  quiet curl -fsSL --retry 3 -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    https://cli.github.com/packages/githubcli-archive-keyring.gpg
  chmod 644 /etc/apt/keyrings/githubcli-archive-keyring.gpg
  printf 'deb [arch=%s signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main\n' \
    "$(dpkg --print-architecture)" >/etc/apt/sources.list.d/github-cli.list
  apt_get update
  local packages=(git gh jq tar podman uidmap slirp4netns dbus-user-session ufw unattended-upgrades)
  if apt-cache show passt >/dev/null 2>&1; then
    packages+=(passt)
  fi
  apt_get install "${packages[@]}"
}

step_firewall() {
  say "Turning on the firewall (SSH on $DIFU_SSH_PORT and difu on $DIFU_PORT)"
  quiet ufw default deny incoming
  quiet ufw default allow outgoing
  quiet ufw allow "$DIFU_SSH_PORT/tcp"
  quiet ufw allow "$DIFU_PORT/tcp"
  if [ "$DIFU_TAILSCALE" = 1 ]; then
    quiet ufw allow in on tailscale0
  fi
  quiet ufw --force enable
}

step_updates() {
  say "Turning on automatic security updates"
  cat >/etc/apt/apt.conf.d/20auto-upgrades <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
EOF
  quiet systemctl enable --now unattended-upgrades.service
}

step_tailscale() {
  say "Joining Tailscale"
  if ! command -v tailscale >/dev/null; then
    quiet curl -fsSL --retry 3 -o "$WORK/tailscale.sh" https://tailscale.com/install.sh
    quiet sh "$WORK/tailscale.sh"
  fi
  quiet systemctl enable --now tailscaled
  if ! tailscale status >/dev/null 2>&1; then
    say "Open the login link below to add this server to your tailnet."
    tailscale up
  fi
  say "Tailscale address: $(tailscale ip -4 | head -n 1)"
}

# Runs over Tailscale once this computer reached the server there.
step_lockdown() {
  say "Closing the public ports: SSH and difu now answer only over Tailscale"
  quiet ufw allow in on tailscale0
  ufw delete allow "$DIFU_SSH_PORT/tcp" >>"$LOG" 2>&1 || true
  ufw delete allow "$DIFU_PORT/tcp" >>"$LOG" 2>&1 || true
}

step_difu() {
  say "Installing difu $DIFU_VERSION"
  local base="https://github.com/opencx-labs/difu/releases/download/v$DIFU_VERSION"
  local archive="difu-$DIFU_VERSION-$DIFU_TARGET.tar.gz"
  if ! curl -fsSL --retry 3 -o "$WORK/$archive" "$base/$archive" 2>>"$LOG" ||
    ! curl -fsSL --retry 3 -o "$WORK/SHA256SUMS" "$base/SHA256SUMS" 2>>"$LOG"; then
    fail "Could not download difu $DIFU_VERSION for $DIFU_TARGET from GitHub. Servers install published releases only: update difu on this computer to a released version, then try again."
  fi
  local sum
  sum=$(awk -v name="$archive" '$2 == name { print $1 }' "$WORK/SHA256SUMS")
  [ -n "$sum" ] || fail "The difu $DIFU_VERSION release lists no checksum for $archive."
  printf '%s  %s\n' "$sum" "$WORK/$archive" | sha256sum --check --status ||
    fail "The downloaded difu archive does not match the release checksum; nothing was installed. Try again."
  mkdir -p "$WORK/difu"
  tar -xzf "$WORK/$archive" -C "$WORK/difu"
  install -m 0755 "$WORK/difu/difu" /usr/local/bin/difu
  local installed
  installed=$(/usr/local/bin/difu --version)
  [ "$installed" = "difu $DIFU_VERSION" ] || fail "Installed difu reports \"$installed\" instead of difu $DIFU_VERSION."
}

step_tools() {
  say "Installing or updating Codex and Claude Code for the difu user"
  local home
  home=$(difu_home)
  as_difu mkdir -p "$home/.local/bin"
  # Codex: OpenAI's static Linux build from its latest GitHub release. Its
  # targets match difu's; the archive holds one binary named after the target.
  local codex="codex-$DIFU_TARGET"
  if try curl -fsSL --retry 3 -o "$WORK/codex.tar.gz" \
    "https://github.com/openai/codex/releases/latest/download/$codex.tar.gz"; then
    mkdir -p "$WORK/codex"
    tar -xzf "$WORK/codex.tar.gz" -C "$WORK/codex"
    [ -f "$WORK/codex/$codex" ] || fail "The Codex release archive has no $codex binary."
    install -m 0755 -o difu -g "$(id -gn difu)" "$WORK/codex/$codex" "$home/.local/bin/codex"
  elif [ -x "$home/.local/bin/codex" ]; then
    say "Warning: could not download the latest Codex (details in $LOG); keeping the installed one."
  else
    fail "Could not download Codex from GitHub (details in $LOG). Check the server's internet access and try again."
  fi
  quiet as_difu "$home/.local/bin/codex" --version
  # Claude Code: Anthropic's native installer, then its own updater.
  if [ ! -x "$home/.local/bin/claude" ]; then
    quiet as_claude bash -c 'set -o pipefail; curl -fsSL https://claude.ai/install.sh | bash'
    [ -x "$home/.local/bin/claude" ] || fail "The Claude Code installer finished without $home/.local/bin/claude."
  elif ! try as_claude "$home/.local/bin/claude" update; then
    say "Warning: Claude Code could not update itself (details in $LOG); keeping the installed version."
  fi
  say "Codex: $(as_difu "$home/.local/bin/codex" --version 2>/dev/null | head -n 1 || true)"
  say "Claude Code: $(as_claude "$home/.local/bin/claude" --version 2>/dev/null | head -n 1 || true)"
  share_claude_config "$home"
}

# The service and every sandbox run Claude Code with CLAUDE_CONFIG_DIR=~/.claude
# (one .claude.json, lock and temporary files); logins over SSH do too.
share_claude_config() {
  local profile="$1/.profile"
  # shellcheck disable=SC2016 # $HOME expands when the profile runs.
  local line='export CLAUDE_CONFIG_DIR="$HOME/.claude"'
  [ -e "$profile" ] || as_difu touch "$profile"
  if ! grep -qxF "$line" "$profile"; then
    printf '\n# Claude Code shares one config with difu-agent and its sandboxes.\n%s\n' "$line" >>"$profile"
  fi
}

# Rootless Podman for the difu user: subordinate ids, a lingering systemd user
# instance, the sandbox image when enabled, and the setting difu reads.
step_sandbox() {
  local file start
  for file in /etc/subuid /etc/subgid; do
    touch "$file"
    if ! grep -q '^difu:' "$file"; then
      start=$(awk -F: 'BEGIN { end = 100000 } $2 + $3 > end { end = $2 + $3 } END { print end }' "$file")
      printf 'difu:%s:65536\n' "$start" >>"$file"
    fi
  done
  local uid home
  uid=$(id -u difu)
  home=$(difu_home)
  quiet loginctl enable-linger difu
  quiet systemctl start "user@$uid.service"
  local _
  for _ in $(seq 1 30); do
    [ -d "/run/user/$uid" ] && break
    sleep 1
  done
  [ -d "/run/user/$uid" ] || fail "The difu user's runtime directory /run/user/$uid did not appear."
  local enabled=false
  if [ "$DIFU_SANDBOX" = 1 ]; then
    say "Building the sandbox image (a few minutes)"
    quiet as_difu /usr/local/bin/difu sandbox build
    enabled=true
  fi
  as_difu mkdir -p "$home/.config/difu"
  chmod 700 "$home/.config/difu"
  # Same lock file as difu's own settings writes.
  quiet as_difu flock "$home/.config/difu/config.json.lock" sh -c '
    umask 077
    file=$1
    [ -s "$file" ] || printf "{}\n" >"$file"
    jq --argjson enabled "$2" ".server.sandbox.enabled = \$enabled" "$file" >"$file.tmp"
    mv "$file.tmp" "$file"
  ' sh "$home/.config/difu/config.json" "$enabled"
}

# Whether the server runs sessions in sandboxes, from difu's own settings.
sandbox_enabled() {
  [ -s "$1" ] && [ "$(jq -r '.server.sandbox.enabled // false' "$1" 2>/dev/null)" = true ]
}

# `difu server upgrade`: the bundled Containerfile may have changed, so rebuild
# the sandbox image (or pull a custom one) when sandboxing is on. Unlike the
# sandbox step, this never changes whether sandboxing is on.
step_image() {
  local home
  home=$(difu_home)
  if ! sandbox_enabled "$home/.config/difu/config.json"; then
    say "Sandboxing is off; no sandbox image to rebuild"
    return
  fi
  say "Rebuilding the sandbox image (a few minutes); running sessions keep theirs"
  quiet as_difu /usr/local/bin/difu sandbox build
  # The replaced image stays while running sandboxes use it.
  try as_difu podman image prune -f || true
}

step_service() {
  say "Installing difu-agent.service"
  local uid home unit
  uid=$(id -u difu)
  home=$(difu_home)
  as_difu mkdir -p "$home/.config/difu"
  unit=${DIFU_UNIT//@UID@/$uid}
  unit=${unit//@HOME@/$home}
  printf '%s\n' "$unit" >/etc/systemd/system/difu-agent.service
  quiet systemctl daemon-reload
  quiet systemctl enable difu-agent.service
  restart_service
}

# Part of the service step: restarts difu-agent and checks that it stays up.
restart_service() {
  say "Starting difu"
  quiet systemctl restart difu-agent.service
  local _
  for _ in $(seq 1 15); do
    systemctl is-active --quiet difu-agent.service && break
    sleep 1
  done
  # Restart=always hides a crash loop from a single check.
  sleep 2
  if ! systemctl is-active --quiet difu-agent.service; then
    journalctl -u difu-agent.service -n 30 --no-pager 2>&1 | sed 's/^/        /' >&2 || true
    fail "difu-agent.service is not running (its log is above)."
  fi
}

main() {
  require_debian
  touch "$LOG"
  chmod 600 "$LOG"
  WORK=$(mktemp -d)
  trap 'rm -rf "$WORK"' EXIT
  trap 'fail "Setup stopped at line $LINENO (details in $LOG)."' ERR
  local step
  for step in $DIFU_STEPS; do
    case "$step" in
      keys | ssh | packages | firewall | updates | tailscale | lockdown | difu | tools | sandbox | image | service)
        "step_$step"
        ;;
      *) fail "Unknown setup step: $step" ;;
    esac
  done
}

# Commands inside main read /dev/null, never the rest of this script on stdin.
main </dev/null
