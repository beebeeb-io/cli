# Beebeeb installer update policy. Inserted into cargo-dist's generated shell
# installer by customize-installer.py; keep this code compatible with /bin/sh.
beebeeb_version() {
    BB_NO_UPDATE=1 "$1" --version 2>/dev/null
}

beebeeb_verify_install() {
    local _bb_version
    _bb_version="$(beebeeb_version "$1")" || err "cannot run installed Beebeeb at $1"
    [ "$_bb_version" = "bb $APP_VERSION" ] || err "update did not take effect at $1: expected bb $APP_VERSION, got $_bb_version"
}

beebeeb_prepare_install() {
    BEEBEEB_EXISTING_BB=""
    # A caller-selected destination is intentional, even if another install is
    # earlier in PATH. Keep cargo-dist's documented destination semantics.
    if [ -n "${BEEBEEB_CLI_INSTALL_DIR:-}${CARGO_DIST_FORCE_INSTALL_DIR:-}${UNMANAGED_INSTALL:-}" ]; then
        return 0
    fi
    local _bb_active
    local _bb_version
    local _bb_brew_prefix
    local _bb_managed=0
    _bb_active="$(command -v bb 2>/dev/null)" || return 0
    [ -f "$_bb_active" ] || err "bb in PATH is not an executable file; choose BEEBEEB_CLI_INSTALL_DIR explicitly"
    _bb_active="$(cd -P "$(dirname "$_bb_active")" && pwd)/$(basename "$_bb_active")"
    _bb_version="$(beebeeb_version "$_bb_active")" || err "cannot identify existing bb at $_bb_active; choose BEEBEEB_CLI_INSTALL_DIR explicitly"
    case "$_bb_version" in
        "bb "[0-9]*.[0-9]*.[0-9]*) ;;
        *) err "existing bb at $_bb_active is not Beebeeb; choose BEEBEEB_CLI_INSTALL_DIR explicitly" ;;
    esac
    BEEBEEB_EXISTING_BB="$_bb_active"
    if command -v brew >/dev/null 2>&1; then
        _bb_brew_prefix="$(brew --prefix 2>/dev/null)" || _bb_brew_prefix=""
        if [ -d "$_bb_brew_prefix" ]; then
            _bb_brew_prefix="$(cd -P "$_bb_brew_prefix" && pwd)"
        fi
        case "$_bb_active" in
            "$_bb_brew_prefix/bin/bb"|"$_bb_brew_prefix"/Cellar/bb/*/bin/bb)
                if [ -n "$_bb_brew_prefix" ] && brew list --formula beebeeb-io/tap/bb >/dev/null 2>&1; then
                    _bb_managed=1
                fi
                ;;
        esac
    fi
    if [ "$_bb_managed" = 1 ]; then
        say "updating Beebeeb via Homebrew"
        ensure brew update --quiet
        ensure brew upgrade beebeeb-io/tap/bb
        # An older self-updater could leave a regular file at prefix/bin/bb.
        # Restore the package-manager link rather than writing into its Cellar.
        ensure brew link --overwrite beebeeb-io/tap/bb
        beebeeb_verify_install "$_bb_active"
        say "bb $APP_VERSION is installed at $_bb_active"
        exit 0
    fi
    # Do not replace a symlink owned by another package manager or pointing into
    # an unknown location. Tell the user how to select a separate installation.
    [ ! -L "$_bb_active" ] || err "existing bb is a symlink at $_bb_active; update it with its package manager or choose BEEBEEB_CLI_INSTALL_DIR explicitly"
    [ -w "$(dirname "$_bb_active")" ] || err "cannot update $_bb_active: directory is not writable; update it with its package manager or choose BEEBEEB_CLI_INSTALL_DIR explicitly"
    UNMANAGED_INSTALL="$(dirname "$_bb_active")"
    NO_MODIFY_PATH=1
    say "updating existing Beebeeb at $_bb_active"
}
