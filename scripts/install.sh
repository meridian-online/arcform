#!/bin/sh
#
# arc installer
# https://meridian.online
#
# Resolves this machine's release target from `uname`, downloads that target's
# arc archive from the GitHub release, checks it against the release's .sha256
# sidecar, unpacks it into ~/.arcform/cli/<version>, points
# ~/.arcform/cli/latest at that directory, and links `arc` into ~/.local/bin.
#
# Usage:
#   curl -fsSL https://install.meridian.online/arc | sh
#   curl -fsSL https://install.meridian.online/arc | sh -s -- v0.2.0
#   sh scripts/install.sh --self-test    # no network, nothing installed
#
# POSIX sh rather than bash, so it runs the same under dash, bash and zsh's sh.
# release.yml sends this file to the install site on every release.
set -eu

REPO="meridian-online/arcform"
BINARY="arc"
# The DuckDB release arc is built and tested against. arc's engine shells out to
# the `duckdb` command for a protocol's SQL steps, so a user needs it on PATH.
DUCKDB_RECOMMENDED="v1.5.6"
SOURCE_INSTALL="cargo install arcform --locked --features mcp,bundled-duckdb"
# FOR TESTS ONLY, not a user setting: --self-test sets this, inside a subshell,
# to a file:// tree laid out as a release is, so the install path runs without
# the network. It is cleared here rather than read from the environment, so a
# variable in the caller's environment cannot point a real install at another
# host, whose own sidecar would make the checksum meaningless.
ARC_INSTALL_BASE_URL=""

info() { printf '  %s\n' "$*"; }
warn() { printf '  warning: %s\n' "$*" >&2; }
error() {
  printf '  error: %s\n' "$*" >&2
  exit 1
}

need_cmd() {
  command -v "$1" > /dev/null 2>&1 || error "need '$1' (command not found)"
}

# The release target for a `uname -s` / `uname -m` pair. Releases are built for
# macOS and Linux (glibc), x86_64 and aarch64; anything else builds from source.
target_for() {
  case "$1" in
    Darwin) tf_os="apple-darwin" ;;
    Linux) tf_os="unknown-linux-gnu" ;;
    *) error "arc's release binaries are for macOS and Linux, not $1. Build it from source instead: ${SOURCE_INSTALL}" ;;
  esac
  case "$2" in
    x86_64 | amd64) tf_arch="x86_64" ;;
    aarch64 | arm64) tf_arch="aarch64" ;;
    *) error "arc's release binaries are for x86_64 and aarch64, not $2. Build it from source instead: ${SOURCE_INSTALL}" ;;
  esac
  printf '%s-%s\n' "$tf_arch" "$tf_os"
}

detect_target() {
  dt_os="$(uname -s)"
  dt_arch="$(uname -m)"
  # A shell running under Rosetta on Apple silicon reports x86_64; the native
  # binary is the one to install there.
  if [ "$dt_os" = "Darwin" ] && [ "$dt_arch" = "x86_64" ] \
    && [ "$(sysctl -n sysctl.proc_translated 2> /dev/null || echo 0)" = "1" ]; then
    dt_arch="arm64"
  fi
  target_for "$dt_os" "$dt_arch"
}

# `v0.2.0` or `0.2.0` becomes `v0.2.0`; anything that is not three
# dot-separated numbers is refused before a request is made.
normalise_version() {
  case "$1" in
    v*) nv="$1" ;;
    *) nv="v$1" ;;
  esac
  nv_rest="${nv#v}"
  case "$nv_rest" in
    '' | *[!0-9.]* | .* | *. | *..*)
      error "'$1' is not a release version; give one like v0.2.0, or none for the latest"
      ;;
  esac
  if [ "$(printf '%s' "$nv_rest" | tr -cd . | wc -c | tr -d ' ')" != "2" ]; then
    error "'$1' is not a release version; give one like v0.2.0, or none for the latest"
  fi
  printf '%s\n' "$nv"
}

latest_version() {
  lv_tag="$(curl -fsSL --proto '=https' --tlsv1.2 "https://api.github.com/repos/${REPO}/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$lv_tag" ] || error "could not read the latest release of ${REPO} from GitHub; name a version instead, like: sh -s -- v0.2.0"
  normalise_version "$lv_tag"
}

# Sets base_url and curl_proto for VERSION: the GitHub release over HTTPS, or the
# self-test's local tree.
release_source() {
  base_url="https://github.com/${REPO}/releases/download/$1"
  curl_proto="=https"
  if [ -n "$ARC_INSTALL_BASE_URL" ]; then
    base_url="${ARC_INSTALL_BASE_URL}/$1"
    curl_proto="=https,file"
  fi
}

archive_name() {
  printf '%s-%s-%s.tar.gz\n' "$BINARY" "$1" "$2"
}

sha256_of() {
  if command -v shasum > /dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    error "need shasum or sha256sum to check the download"
  fi
}

# Refuses unless ARCHIVE hashes to the first field of SIDECAR, which the
# release writes in `hash  filename` form.
verify_checksum() {
  [ -f "$2" ] || error "no checksum file for $(basename "$1")"
  vc_expected="$(awk 'NR == 1 {print $1}' "$2")"
  [ -n "$vc_expected" ] || error "the checksum file for $(basename "$1") is empty"
  vc_actual="$(sha256_of "$1")"
  if [ "$vc_expected" != "$vc_actual" ]; then
    error "checksum mismatch for $(basename "$1"): the release says ${vc_expected}, the download is ${vc_actual}. Nothing was installed."
  fi
}

# post_install_warnings LOCAL_BIN LINKED: LINKED is "no" when another arc in
# LOCAL_BIN was left alone, and then adding LOCAL_BIN to PATH would find that
# one, so the hint is not given.
post_install_warnings() {
  case ":${PATH}:" in
    *":$1:"*) ;;
    *)
      if [ "${2:-yes}" != "no" ]; then
        warn "$1 is not on your PATH, so your shell will not find arc yet. Add this line to your shell's startup file (~/.zshrc, ~/.bashrc):"
        warn "  export PATH=\"\$HOME/.local/bin:\$PATH\""
      fi
      ;;
  esac
  if ! command -v duckdb > /dev/null 2>&1; then
    warn "the duckdb command is not on your PATH. arc runs a protocol's SQL steps with it, so install DuckDB ${DUCKDB_RECOMMENDED}: https://duckdb.org/docs/installation/"
  fi
}

main() {
  need_cmd uname
  need_cmd curl
  need_cmd tar
  need_cmd mktemp
  [ -n "${HOME:-}" ] || error "HOME is not set, so there is nowhere to install to"

  install_root="${HOME}/.arcform/cli"
  local_bin="${HOME}/.local/bin"

  target="$(detect_target)"
  if [ -n "${1:-}" ]; then
    version="$(normalise_version "$1")"
  else
    version="$(latest_version)"
  fi
  archive="$(archive_name "$version" "$target")"
  release_source "$version"

  echo ""
  echo "  arc installer (Meridian)"
  echo ""
  info "installing arc ${version} for ${target}"

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT
  trap 'rm -rf "$tmp_dir"; exit 130' INT
  trap 'rm -rf "$tmp_dir"; exit 143' TERM

  info "downloading ${archive}"
  curl -fsSL --proto "$curl_proto" --tlsv1.2 -o "${tmp_dir}/${archive}" "${base_url}/${archive}" \
    || error "could not download ${base_url}/${archive}. Is ${version} released for ${target}?"
  curl -fsSL --proto "$curl_proto" --tlsv1.2 -o "${tmp_dir}/${archive}.sha256" "${base_url}/${archive}.sha256" \
    || error "could not download the checksum for ${archive}"
  verify_checksum "${tmp_dir}/${archive}" "${tmp_dir}/${archive}.sha256"
  info "checksum verified"

  version_dir="${install_root}/${version}"
  mkdir -p "$version_dir"
  tar -xzf "${tmp_dir}/${archive}" -C "$version_dir" "$BINARY"
  chmod +x "${version_dir}/${BINARY}"
  ln -sfn "$version_dir" "${install_root}/latest"
  info "installed ${version_dir}/${BINARY}"
  info "${install_root}/latest -> ${version_dir}"

  # ~/.local/bin/arc is replaced only when it is this installer's own link. Any
  # other file there, or a link to somewhere else, is someone else's `arc` and is
  # left alone: the install has still succeeded, under ~/.arcform/cli/latest.
  mkdir -p "$local_bin"
  link="${local_bin}/${BINARY}"
  linked=yes
  if [ -L "$link" ] || [ -e "$link" ]; then
    existing=""
    if [ -L "$link" ]; then
      existing="$(readlink "$link")"
    fi
    case "$existing" in
      "${install_root}"/*) ;;
      *)
        if [ -n "$existing" ]; then
          warn "${link} already exists and links to ${existing}, not to this install, so it was left alone."
        else
          warn "${link} already exists and is not this installer's link, so it was left alone."
        fi
        warn "Run arc as ${install_root}/latest/${BINARY}, or remove ${link} and run this installer again."
        link=""
        linked=no
        ;;
    esac
  fi
  if [ -n "$link" ]; then
    ln -sf "${install_root}/latest/${BINARY}" "$link"
    info "linked ${link}"
  fi

  if "${version_dir}/${BINARY}" --version > /dev/null 2>&1; then
    info "$("${version_dir}/${BINARY}" --version) is ready"
  else
    warn "the installed arc did not start. On Linux the release binaries need glibc 2.35 or later (Ubuntu 22.04, Debian 12 or newer); on an older system, build from source: ${SOURCE_INSTALL}"
  fi
  echo ""
  post_install_warnings "$local_bin" "$linked"
}

# ---------------------------------------------------------------------------
# --self-test: target resolution, version handling, the checksum refusal, and
# the whole install run against a local release tree with a temporary HOME.
# No network, and nothing written outside a temporary directory.
# ---------------------------------------------------------------------------

st_passed=0
st_failed=0
st_pass() { st_passed=$((st_passed + 1)); }
st_fail() {
  st_failed=$((st_failed + 1))
  printf '  FAILED %s\n' "$*" >&2
}
st_equal() {
  if [ "$2" = "$3" ]; then st_pass; else st_fail "$1: wanted '$2', got '$3'"; fi
}
# st_refuses LABEL NEEDLE COMMAND...: the command fails, and says NEEDLE.
st_refuses() {
  st_label="$1"
  st_needle="$2"
  shift 2
  if st_out="$( ("$@") 2>&1)"; then
    st_fail "${st_label}: accepted (printed '${st_out}')"
  else
    case "$st_out" in
      *"$st_needle"*) st_pass ;;
      *) st_fail "${st_label}: refused, but without '${st_needle}': ${st_out}" ;;
    esac
  fi
}

# st_install HOME SITE LOG: main() for v0.0.1 with set -e in force, outside any
# conditional (where the shell would ignore it); its exit status in st_rc.
st_install() {
  set +e
  (
    set -e
    HOME="$1"
    ARC_INSTALL_BASE_URL="file://$2"
    main v0.0.1
  ) > "$3" 2>&1
  st_rc=$?
  set -e
}

# The install itself: main() against a file:// release tree laid out as
# release.yml publishes one (`arc` at the archive's root, a `hash  filename`
# sidecar beside it), with HOME in a temporary directory.
st_end_to_end() {
  if ! st_target="$( (detect_target) 2> /dev/null)"; then
    st_fail "end to end: this machine has no release target"
    return 0
  fi
  st_e2e="$(mktemp -d)"
  st_version="v0.0.1"
  st_site="${st_e2e}/site"
  st_archive="$(archive_name "$st_version" "$st_target")"
  mkdir -p "${st_e2e}/pkg" "${st_site}/${st_version}"
  printf '#!/bin/sh\necho "arc 0.0.1"\n' > "${st_e2e}/pkg/arc"
  chmod +x "${st_e2e}/pkg/arc"
  tar -C "${st_e2e}/pkg" -czf "${st_site}/${st_version}/${st_archive}" arc
  printf '%s  %s\n' "$(sha256_of "${st_site}/${st_version}/${st_archive}")" "$st_archive" \
    > "${st_site}/${st_version}/${st_archive}.sha256"

  st_home="${st_e2e}/home"
  mkdir -p "$st_home"
  # Not inside `if`, `&&` or `||`: there the shell ignores `set -e`, even one
  # set inside the subshell, and a failing step in main would go unnoticed.
  set +e
  (
    set -e
    HOME="$st_home"
    ARC_INSTALL_BASE_URL="file://${st_site}"
    main "$st_version"
  ) > "${st_e2e}/install.log" 2>&1
  st_rc=$?
  set -e
  st_equal "end to end: the install exits 0 under set -e ($(cat "${st_e2e}/install.log"))" "0" "$st_rc"
  st_equal "end to end: latest is the installed version" \
    "$(cd "${st_home}/.arcform/cli/${st_version}" 2> /dev/null && pwd -P)" \
    "$(cd "${st_home}/.arcform/cli/latest" 2> /dev/null && pwd -P)"
  st_equal "end to end: ~/.local/bin/arc runs" "arc 0.0.1" "$("${st_home}/.local/bin/arc" --version 2>&1)"

  # Someone else's `arc` in ~/.local/bin, a regular file: left byte for byte, with
  # a warning, and the install still succeeds.
  st_home3="${st_e2e}/home3"
  mkdir -p "${st_home3}/.local/bin"
  printf "someone else's arc\n" > "${st_home3}/.local/bin/arc"
  cp "${st_home3}/.local/bin/arc" "${st_e2e}/other-arc"
  st_install "$st_home3" "${st_site}" "${st_e2e}/other.log"
  st_equal "end to end, another arc present: the install exits 0" "0" "$st_rc"
  if cmp -s "${st_home3}/.local/bin/arc" "${st_e2e}/other-arc" && [ ! -L "${st_home3}/.local/bin/arc" ]; then
    st_pass
  else
    st_fail "end to end: another arc in ~/.local/bin was replaced"
  fi
  case "$(cat "${st_e2e}/other.log")" in
    *"so it was left alone"*) st_pass ;;
    *) st_fail "end to end: no warning that another arc was left alone: $(cat "${st_e2e}/other.log")" ;;
  esac
  # ...and no hint to put ~/.local/bin on PATH, which would find the other arc.
  case "$(cat "${st_e2e}/other.log")" in
    *"is not on your PATH, so your shell will not find arc"*) st_fail "end to end: told to put ~/.local/bin on PATH with another arc there" ;;
    *) st_pass ;;
  esac

  # A link to some other program: also left alone.
  st_home5="${st_e2e}/home5"
  mkdir -p "${st_home5}/.local/bin"
  ln -s /bin/sh "${st_home5}/.local/bin/arc"
  st_install "$st_home5" "${st_site}" "${st_e2e}/other-link.log"
  st_equal "end to end, a link elsewhere: left alone" "/bin/sh" "$(readlink "${st_home5}/.local/bin/arc")"

  # This installer's own link from an earlier install: replaced, as before.
  st_home4="${st_e2e}/home4"
  mkdir -p "${st_home4}/.local/bin"
  ln -s "${st_home4}/.arcform/cli/v0.0.0/arc" "${st_home4}/.local/bin/arc"
  st_install "$st_home4" "${st_site}" "${st_e2e}/own-link.log"
  st_equal "end to end, its own earlier link: replaced" \
    "${st_home4}/.arcform/cli/latest/arc" "$(readlink "${st_home4}/.local/bin/arc")"

  # The same release with its sidecar tampered with: refused, nothing installed.
  printf '%s  %s\n' "0000000000000000000000000000000000000000000000000000000000000000" "$st_archive" \
    > "${st_site}/${st_version}/${st_archive}.sha256"
  st_home2="${st_e2e}/home2"
  mkdir -p "$st_home2"
  set +e
  (
    set -e
    HOME="$st_home2"
    ARC_INSTALL_BASE_URL="file://${st_site}"
    main "$st_version"
  ) > "${st_e2e}/tampered.log" 2>&1
  st_rc=$?
  set -e
  if [ "$st_rc" -eq 0 ]; then
    st_fail "end to end: a tampered checksum was installed"
  else
    case "$(cat "${st_e2e}/tampered.log")" in
      *"checksum mismatch"*) st_pass ;;
      *) st_fail "end to end: refused, but not for the checksum: $(cat "${st_e2e}/tampered.log")" ;;
    esac
  fi
  if [ -e "${st_home2}/.arcform" ] || [ -e "${st_home2}/.local" ]; then
    st_fail "end to end: a refused install left files in HOME"
  else
    st_pass
  fi
  rm -rf "$st_e2e"
}

self_test() {
  # Each supported `uname -s` / `uname -m` pair, and the target it names.
  for st_case in \
    "Darwin arm64 aarch64-apple-darwin" \
    "Darwin aarch64 aarch64-apple-darwin" \
    "Darwin x86_64 x86_64-apple-darwin" \
    "Linux x86_64 x86_64-unknown-linux-gnu" \
    "Linux amd64 x86_64-unknown-linux-gnu" \
    "Linux aarch64 aarch64-unknown-linux-gnu" \
    "Linux arm64 aarch64-unknown-linux-gnu"; do
    # shellcheck disable=SC2086 # split the case into its three words
    set -- $st_case
    st_equal "target for $1 $2" "$3" "$( (target_for "$1" "$2") 2>&1)"
  done

  # Every other system is refused, and pointed at a source build.
  for st_case in "FreeBSD amd64" "MINGW64_NT-10.0 x86_64" "SunOS i86pc" "Linux armv7l" "Darwin i386" "Linux riscv64"; do
    # shellcheck disable=SC2086
    set -- $st_case
    st_refuses "refuse $1 $2" "cargo install arcform" target_for "$1" "$2"
  done

  st_equal "the archive name" "arc-v0.2.0-aarch64-apple-darwin.tar.gz" "$(archive_name v0.2.0 aarch64-apple-darwin)"
  st_equal "a version without its v" "v0.2.0" "$( (normalise_version 0.2.0) 2>&1)"
  st_equal "a version with its v" "v0.2.0" "$( (normalise_version v0.2.0) 2>&1)"
  for st_bad in "latest" "v0.2" "v0.2.0-rc1" "v0.2.0.1" "v0..2" "v.2.0" "vX.Y.Z" "v0.2.0
v0.3.0"; do
    st_refuses "refuse version '${st_bad}'" "is not a release version" normalise_version "$st_bad"
  done

  # The hash is SHA-256, read off a known value.
  st_dir="$(mktemp -d)"
  printf 'abc' > "${st_dir}/abc"
  st_equal "sha256 of 'abc'" "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" "$(sha256_of "${st_dir}/abc")"

  # A local archive against a matching sidecar, a mismatched one and none.
  st_archive="${st_dir}/arc-v0.2.0-x86_64-unknown-linux-gnu.tar.gz"
  printf 'not really an archive\n' > "$st_archive"
  printf '%s  %s\n' "$(sha256_of "$st_archive")" "$(basename "$st_archive")" > "${st_dir}/good.sha256"
  printf '%s  %s\n' "0000000000000000000000000000000000000000000000000000000000000000" "$(basename "$st_archive")" > "${st_dir}/bad.sha256"
  if (verify_checksum "$st_archive" "${st_dir}/good.sha256") > /dev/null 2>&1; then
    st_pass
  else
    st_fail "a matching checksum was refused"
  fi
  st_refuses "a mismatched checksum" "checksum mismatch" verify_checksum "$st_archive" "${st_dir}/bad.sha256"
  st_refuses "a missing checksum file" "no checksum file" verify_checksum "$st_archive" "${st_dir}/absent.sha256"
  rm -rf "$st_dir"

  st_end_to_end

  # A real install ignores ARC_INSTALL_BASE_URL from the environment: a fresh
  # run of this script, with it exported, still downloads from GitHub over HTTPS.
  st_equal "an exported ARC_INSTALL_BASE_URL is ignored" \
    "https://github.com/${REPO}/releases/download/v0.0.1 =https" \
    "$(ARC_INSTALL_BASE_URL="file:///tmp/elsewhere" sh "$0" --release-source v0.0.1 2>&1)"

  printf 'install.sh self-test: %s passed, %s failed\n' "$st_passed" "$st_failed"
  [ "$st_failed" -eq 0 ]
}

if [ "${1:-}" = "--self-test" ]; then
  self_test
  exit $?
fi
# For the self-test only: print where VERSION would be downloaded from.
if [ "${1:-}" = "--release-source" ]; then
  release_source "$2"
  printf '%s %s\n' "$base_url" "$curl_proto"
  exit 0
fi
main "$@"
