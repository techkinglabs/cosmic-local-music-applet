#!/usr/bin/env bash
set -euo pipefail

APP_NAME="cosmic-media-applet"
APP_ID="com.system76.CosmicMediaApplet"
BIN_NAME="cosmic-media-applet"
DESKTOP_FILE="${APP_ID}.desktop"
BUILD_DIR="target/release"
USAGE="Usage: $0 [--uninstall] [--prefix /path/to/prefix] [--build] [--local]"

RESET='\033[0m'
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'

log_info()  { echo -e "${BLUE}[INFO]${RESET}  $*"; }
log_ok()    { echo -e "${GREEN}[OK]${RESET}    $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${RESET}  $*"; }
log_error() { echo -e "${RED}[ERROR]${RESET} $*" >&2; }
log_step()  { echo -e "${BOLD}$*${RESET}"; }

die() {
    log_error "$1"
    exit 1
}

UNINSTALL_MODE=false
PREFIX=""
FORCE_BUILD=false
REQUIRE_SUDO=false
LOCAL_BINARY=false

determine_sudo_needed() {
    if [[ -z "${PREFIX}" ]] || [[ "${PREFIX}" == "/" ]]; then
        REQUIRE_SUDO=true
    else
        REQUIRE_SUDO=false
    fi
}

may_run() {
    if [[ "${REQUIRE_SUDO}" == true ]] && [[ "$(id -u)" -ne 0 ]] && command -v sudo &>/dev/null; then
        echo "sudo"
    else
        echo ""
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --uninstall|-u)
            UNINSTALL_MODE=true
            shift
            ;;
        --prefix|-p)
            [[ $# -ge 2 ]] || die "Option $1 requires an argument"
            PREFIX="$2"
            shift 2
            ;;
        --build|-b)
            FORCE_BUILD=true
            shift
            ;;
        --local|--user)
            LOCAL_BINARY=true
            shift
            ;;
        --help|-h)
            echo "$USAGE"
            exit 0
            ;;
        *)
            die "Unknown option: $1"
            ;;
    esac
done

SYSTEM_DESKTOP_DIR="/usr/share/applications"
SYSTEM_BIN_DIR="/usr/bin"
SYSTEM_LIB_DIR="/usr/lib/cosmic-desktop"

preflight_checks() {
    log_step "Running pre-flight checks..."

    determine_sudo_needed
    local SUDO="$(may_run)"

    if [[ "${REQUIRE_SUDO}" == true ]] && [[ "$(id -u)" -ne 0 ]] && ! command -v sudo &>/dev/null; then
        die "This script requires root privileges (or sudo is not installed)"
    fi

    if [[ "${REQUIRE_SUDO}" == true ]] && [[ "$(id -u)" -ne 0 ]]; then
        log_warn "Not running as root; sudo will be used for privileged operations"
    fi

    if ! command -v cargo &>/dev/null; then
        die "cargo not found in PATH. Install Rust: https://rustup.rs"
    fi
    log_ok "cargo found: $(cargo --version 2>/dev/null)"

    if ! command -v rustc &>/dev/null; then
        die "rustc not found in PATH"
    fi
    log_ok "rustc found: $(rustc --version 2>/dev/null)"

    if [[ ! -f "Cargo.toml" ]]; then
        die "Cargo.toml not found in current directory. Run this from the project root."
    fi
    log_ok "Cargo.toml found"

    if [[ ! -d "src" ]]; then
        die "src/ directory not found in current directory"
    fi
    log_ok "src/ directory found"

    if ! command -v desktop-file-install &>/dev/null; then
        log_warn "desktop-file-install not found; will use cp instead"
    else
        log_ok "desktop-file-install available"
    fi

    if command -v update-desktop-database &>/dev/null; then
        log_ok "update-desktop-database available"
    else
        log_warn "update-desktop-database not found; skipping DB update"
    fi
}

build_project() {
    log_step "Building project..."
    if [[ ! -d "${BUILD_DIR}" ]] || [[ "${FORCE_BUILD}" == true ]]; then
        cargo build --release 2>&1 || die "Build failed with exit code $?"
    else
        log_info "Release build already exists, skipping build (use --build to force)"
    fi

    if [[ ! -x "${BUILD_DIR}/${BIN_NAME}" ]]; then
        die "Build succeeded but binary '${BUILD_DIR}/${BIN_NAME}' not found or not executable"
    fi
    log_ok "Binary ready: ${BUILD_DIR}/${BIN_NAME}"
}

install_files() {
    log_step "Installing files..."
    determine_sudo_needed
    local SUDO="$(may_run)"

    local install_root=""
    if [[ -n "${PREFIX}" ]] && [[ "${PREFIX}" != "/" ]] && [[ "${PREFIX}" != "/usr" ]]; then
        install_root="${PREFIX}"
    fi

    local target_desktop_dir="${install_root}${SYSTEM_DESKTOP_DIR}"
    local target_bin_dir="${install_root}${SYSTEM_BIN_DIR}"
    local target_lib_dir="${install_root}${SYSTEM_LIB_DIR}"

    ${SUDO} mkdir -p "${target_lib_dir}" || die "Failed to create ${target_lib_dir}"

    if [[ ! -d "${target_desktop_dir}" ]]; then
        ${SUDO} mkdir -p "${target_desktop_dir}" || die "Failed to create ${target_desktop_dir}"
    fi

    local target_bin="${target_lib_dir}/${BIN_NAME}"
    ${SUDO} install -m 0755 "${BUILD_DIR}/${BIN_NAME}" "${target_bin}" || die "Failed to install binary"
    log_ok "Binary installed: ${target_bin}"

    local user_bin_dir="${HOME}/.local/bin"
    if [[ -z "${PREFIX}" ]] || [[ "${PREFIX}" == "/" ]]; then
        mkdir -p "${user_bin_dir}" 2>/dev/null || true
        if [[ -d "${user_bin_dir}" ]]; then
            install -m 0755 "${BUILD_DIR}/${BIN_NAME}" "${user_bin_dir}/${BIN_NAME}" \
                || log_warn "Failed to update local binary at ${user_bin_dir}/${BIN_NAME}"
            log_ok "Local binary updated: ${user_bin_dir}/${BIN_NAME}"
        fi
    fi
    if [[ "${LOCAL_BINARY}" == true ]]; then
        mkdir -p "${user_bin_dir}" || die "Failed to create ${user_bin_dir}"
        install -m 0755 "${BUILD_DIR}/${BIN_NAME}" "${user_bin_dir}/${BIN_NAME}" \
            || die "Failed to install local binary"
        log_ok "Local binary installed: ${user_bin_dir}/${BIN_NAME}"
    fi

    if [[ -z "${PREFIX}" ]]; then
        local symlink_path="${target_bin_dir}/${BIN_NAME}"
        if [[ ! -e "${symlink_path}" ]] || [[ ! -L "${symlink_path}" ]]; then
            ${SUDO} ln -sf "../lib/cosmic-desktop/${BIN_NAME}" "${symlink_path}" || log_warn "Failed to create symlink at ${symlink_path}"
            log_ok "Symlink created: ${symlink_path}"
        else
            log_warn "Symlink already exists at ${symlink_path}, skipping"
        fi
    fi

    local desktop_src="${APP_ID}.desktop"
    if [[ ! -f "${desktop_src}" ]]; then
        cat > "${desktop_src}" <<DESKTOP_EOF
[Desktop Entry]
Name=Media
Comment=Universal media controls via MPRIS2
Type=Application
Exec=${BIN_NAME}
Terminal=false
Categories=COSMIC;
Keywords=COSMIC;Applet;Media;MPRIS;Player;
Icon=multimedia-player-symbolic
StartupNotify=true
NoDisplay=true
X-CosmicApplet=true
X-CosmicShrinkable=true
X-CosmicHoverPopup=End
X-OverflowPriority=10
DESKTOP_EOF
        log_info "Generated desktop file: ${desktop_src}"
    fi

    local target_desktop="${target_desktop_dir}/${DESKTOP_FILE}"
    if command -v desktop-file-install &>/dev/null; then
        ${SUDO} desktop-file-install --dir="${target_desktop_dir}" "${desktop_src}" &>/dev/null || true
    fi
    if [[ ! -f "${target_desktop}" ]]; then
        ${SUDO} cp "${desktop_src}" "${target_desktop}" || die "Failed to install desktop file"
        ${SUDO} chmod 0644 "${target_desktop}" || log_warn "Failed to set permissions on desktop file"
    fi
    log_ok "Desktop file installed: ${target_desktop}"

    if command -v update-desktop-database &>/dev/null; then
        ${SUDO} update-desktop-database "${target_desktop_dir}" &>/dev/null || log_warn "Failed to update desktop database"
        log_ok "Desktop database updated"
    fi

    if command -v gtk-update-icon-cache &>/dev/null 2>&1; then
        local icon_dir="${install_root}/usr/share/icons/hicolor"
        ${SUDO} gtk-update-icon-cache -f -t "${icon_dir}" &>/dev/null || true
    fi
}

verify_install() {
    log_step "Verifying installation..."

    local check_failed=false
    local verify_desktop_dir=""
    local verify_lib_dir=""

    local verify_root=""
    if [[ -n "${PREFIX}" ]] && [[ "${PREFIX}" != "/" ]] && [[ "${PREFIX}" != "/usr" ]]; then
        verify_root="${PREFIX}"
    fi

    verify_desktop_dir="${verify_root}${SYSTEM_DESKTOP_DIR}"
    verify_lib_dir="${verify_root}${SYSTEM_LIB_DIR}"

    local target_bin="${verify_lib_dir}/${BIN_NAME}"
    if [[ -x "${target_bin}" ]]; then
        log_ok "Binary found: ${target_bin}"
    else
        log_error "Binary not found at ${target_bin}"
        check_failed=true
    fi

    local target_desktop="${verify_desktop_dir}/${DESKTOP_FILE}"
    if [[ -f "${target_desktop}" ]]; then
        log_ok "Desktop file found: ${target_desktop}"
    else
        log_error "Desktop file not found at ${target_desktop}"
        check_failed=true
    fi

    if [[ -z "${PREFIX}" ]] && [[ ! -L "${SYSTEM_BIN_DIR}/${BIN_NAME}" ]]; then
        log_warn "Symlink not found at ${SYSTEM_BIN_DIR}/${BIN_NAME}"
        check_failed=true
    fi

    if [[ "${check_failed}" == true ]]; then
        die "Installation verification failed"
    fi
}

uninstall_applet() {
    log_step "Uninstalling ${APP_NAME}..."
    determine_sudo_needed
    local SUDO="$(may_run)"

    local uninstall_root=""
    if [[ -n "${PREFIX}" ]] && [[ "${PREFIX}" != "/" ]] && [[ "${PREFIX}" != "/usr" ]]; then
        uninstall_root="${PREFIX}"
    fi

    local target_bin="${uninstall_root}${SYSTEM_LIB_DIR}/${BIN_NAME}"
    local symlink="${uninstall_root}${SYSTEM_BIN_DIR}/${BIN_NAME}"
    local desktop="${uninstall_root}${SYSTEM_DESKTOP_DIR}/${DESKTOP_FILE}"

    if [[ -L "${symlink}" ]]; then
        ${SUDO} rm -f "${symlink}" || log_warn "Failed to remove symlink ${symlink}"
        log_ok "Removed symlink: ${symlink}"
    elif [[ -e "${symlink}" ]]; then
        log_warn "${symlink} exists but is not a symlink; leaving it in place"
    fi

    if [[ -f "${target_bin}" ]]; then
        ${SUDO} rm -f "${target_bin}" || log_warn "Failed to remove binary ${target_bin}"
        log_ok "Removed binary: ${target_bin}"
    else
        log_warn "Binary not found at ${target_bin}"
    fi

    if [[ -f "${desktop}" ]]; then
        ${SUDO} rm -f "${desktop}" || log_warn "Failed to remove desktop file ${desktop}"
        log_ok "Removed desktop file: ${desktop}"
    else
        log_warn "Desktop file not found at ${desktop}"
    fi

    if command -v update-desktop-database &>/dev/null; then
        local desktop_dir="${uninstall_root}${SYSTEM_DESKTOP_DIR}"
        ${SUDO} update-desktop-database "${desktop_dir}" 2>/dev/null || true
    fi

    if [[ -f "${APP_ID}.desktop" ]]; then
        rm -f "${APP_ID}.desktop" || log_warn "Failed to remove generated desktop file"
    fi

    log_ok "Uninstall complete"
}

restart_applet() {
    log_step "Restarting applet..."
    if pgrep -x "cosmic-media-applet" &>/dev/null; then
        pkill -x "cosmic-media-applet" 2>/dev/null || true
        sleep 1
        log_ok "Applet process killed; panel will auto-restart it"
    else
        log_info "Applet not running (will start on next panel refresh)"
    fi
}

main() {
    if [[ "${UNINSTALL_MODE}" == true ]]; then
        uninstall_applet
        exit 0
    fi

    preflight_checks
    build_project
    install_files
    verify_install

    echo ""
    local install_prefix=""
    if [[ -n "${PREFIX}" ]] && [[ "${PREFIX}" != "/" ]] && [[ "${PREFIX}" != "/usr" ]]; then
        install_prefix="${PREFIX}"
    fi
    log_ok "${APP_NAME} installed successfully!"
    echo ""
    echo -e "  ${BOLD}Desktop file:${RESET} ${install_prefix}${SYSTEM_DESKTOP_DIR}/${DESKTOP_FILE}"
    echo -e "  ${BOLD}Binary:${RESET}     ${install_prefix}${SYSTEM_LIB_DIR}/${BIN_NAME}"
    if [[ -z "${PREFIX}" ]] || [[ "${PREFIX}" == "/" ]]; then
        echo -e "  ${BOLD}Symlink:${RESET}    ${SYSTEM_BIN_DIR}/${BIN_NAME}"
    fi
    echo ""
    restart_applet
    echo ""
    echo -e "  ${YELLOW}Next steps:${RESET}"
    echo "  1. Add the applet via Settings -> Desktop -> Panel -> Applets"
    echo "  2. Select 'Media' from the available applets"
    echo ""
}

trap 'log_error "Script interrupted or failed at line $LINENO"; exit 130' INT TERM
main "$@"