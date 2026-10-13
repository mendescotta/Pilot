#!/bin/sh
set -e

MODE=install
SCOPE=system
for arg in "$@"; do
    case "$arg" in
        --uninstall) MODE=uninstall ;;
        --user) SCOPE=user ;;
        *)
            echo "Usage: $0 [--user] [--uninstall]" >&2
            exit 1
            ;;
    esac
done

SRC_DIR="$(cd "$(dirname "$0")" && pwd)"

if [ "$SCOPE" = system ]; then
    if [ "$(id -u)" -ne 0 ]; then
        echo "System-wide mode needs root, e.g.:" >&2
        echo "  sudo $0${MODE:+ --uninstall}" >&2
        echo "...or add --user for a no-root, this-user-only install." >&2
        exit 1
    fi
    PREFIX="${PREFIX:-/usr}"
    BINDIR="$PREFIX/bin"
    LIBEXECDIR="$PREFIX/libexec"
    DATADIR="$PREFIX/share"
else
    if [ "$(id -u)" -eq 0 ]; then
        echo "--user mode shouldn't run as root — it writes into the" >&2
        echo "invoking user's own home directory. Drop --user to install" >&2
        echo "system-wide instead." >&2
        exit 1
    fi
    DATADIR="${XDG_DATA_HOME:-$HOME/.local/share}"
fi

DESKTOP_FILE="$DATADIR/applications/org.voidlab.Pilot.desktop"
APP_ICON="$DATADIR/icons/hicolor/scalable/apps/org.voidlab.Pilot.svg"

refresh_desktop_caches() {
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -f -t "$DATADIR/icons/hicolor" >/dev/null 2>&1 || true
    fi
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DATADIR/applications" >/dev/null 2>&1 || true
    fi
}

if [ "$MODE" = uninstall ]; then
    if [ "$SCOPE" = system ]; then
        rm -fv \
            "$BINDIR/pilot" \
            "$LIBEXECDIR/pilot-helper" \
            "$DATADIR/polkit-1/actions/org.voidlab.Pilot.policy" \
            "$DESKTOP_FILE" \
            "$APP_ICON"
        echo "Not removing $PREFIX/lib/dinit.d/user/boot.d: other packages may rely on it" \
            "(turnstile's dinit backend expects it to exist)."
    else
        rm -fv "$DESKTOP_FILE" "$APP_ICON"
    fi
    refresh_desktop_caches
    echo "Uninstalled ($SCOPE)."
    exit 0
fi

if [ "$SCOPE" = system ]; then
    TARGET_DIR="$SRC_DIR/target/release"
    if [ ! -x "$TARGET_DIR/pilot" ] || [ ! -x "$TARGET_DIR/pilot-helper" ]; then
        echo "Build first: cargo build --release --locked" >&2
        exit 1
    fi

    install -Dm755 "$TARGET_DIR/pilot" "$BINDIR/pilot"
    install -Dm755 "$TARGET_DIR/pilot-helper" "$LIBEXECDIR/pilot-helper"
    install -Dm644 "$SRC_DIR/assets/org.voidlab.Pilot.policy" \
        "$DATADIR/polkit-1/actions/org.voidlab.Pilot.policy"
    install -Dm644 "$SRC_DIR/assets/org.voidlab.Pilot.desktop" "$DESKTOP_FILE"
    install -Dm644 "$SRC_DIR/assets/org.voidlab.Pilot.svg" "$APP_ICON"
    # Turnstile's dinit backend expects this to exist but never creates it itself (it only
    # mkdir -p's the per-user boot dir inside $HOME, not this system-wide one).
    install -dm755 "$PREFIX/lib/dinit.d/user/boot.d"

    refresh_desktop_caches
    echo "Installed. Launch with: pilot"
else
    if [ -x "$SRC_DIR/target/release/pilot" ]; then
        BIN="$SRC_DIR/target/release/pilot"
    elif [ -x "$SRC_DIR/target/debug/pilot" ]; then
        BIN="$SRC_DIR/target/debug/pilot"
    else
        echo "Build first: cargo build (or cargo build --release --locked)" >&2
        exit 1
    fi

    mkdir -p "$(dirname "$DESKTOP_FILE")" "$(dirname "$APP_ICON")"
    sed "s|^Exec=pilot\$|Exec=$BIN|" "$SRC_DIR/assets/org.voidlab.Pilot.desktop" > "$DESKTOP_FILE"
    ln -sf "$SRC_DIR/assets/org.voidlab.Pilot.svg" "$APP_ICON"

    refresh_desktop_caches
    echo "Registered $BIN for this user."
    echo "(Re)launch Pilot — the desktop shell will now show its real icon and name."
    echo "Note: --user mode only wires desktop integration; it can't install the polkit" \
        "policy or privileged helper (both need root), so System-scope actions won't" \
        "authenticate. Use the default system-wide mode for that."
fi
