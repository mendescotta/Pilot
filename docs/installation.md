# Installation and upgrades

Build as a regular user from the Pilot repository, then install system-wide:

```sh
cargo build --release --locked
sudo ./install.sh
pilot
```

The default `/usr` installation contains:

| File | Purpose |
| --- | --- |
| `/usr/bin/pilot` | Unprivileged GTK application |
| `/usr/libexec/pilot-helper` | Privileged service operations |
| `/usr/share/polkit-1/actions/org.voidlab.Pilot.policy` | Helper authorization policy |
| `/usr/share/applications/org.voidlab.Pilot.desktop` | Application menu entry |
| `/usr/share/icons/hicolor/scalable/apps/org.voidlab.Pilot.svg` | Application icon |

Start Pilot from the application menu or run `pilot` as your regular user.
Unlock live status inside the application. Do not launch the GUI with sudo.
Definitions can be browsed before authentication when filesystem permissions
allow it. Installing Pilot does not enable, disable, start or stop services.

## Upgrade or remove

Close Pilot before upgrading so that an old GUI cannot keep using an old helper.
Build the new release, then run `sudo ./install.sh` again. Install the GUI and
helper from the same checkout together: the current helper protocol is **2**.
A protocol mismatch requires reinstalling the matching pair.

If an existing installation belongs to an XBPS package, manage its removal or
upgrade through XBPS before switching to this source installation. The script
does not register a package with XBPS.

To remove the default source installation:

```sh
sudo ./install.sh --uninstall
```

Removal deletes the five installed application files, leaving service
definitions and logs in place. A previous `./install.sh --user` desktop
registration can be removed with `./install.sh --user --uninstall` as that user.

Runner is installed and removed independently. Pilot's helper and policy use
fixed `/usr` paths; use the default prefix for the documented installation.

## Local deployment record: 2026-10-19

On the development workstation, the previous, non-XBPS-owned Pilot was backed
up to `/var/backups/voidlab-apps-HLmlvE1X`, removed, and replaced with the rebuilt
Pilot and matching protocol-2 helper. Runner was installed system-wide from its
independent sibling repository.

All eight installed application files across both projects matched their build
or asset sources and had root ownership with the expected permissions. The
helper returned `READY 2`; both installed GUI applications opened successfully
as the regular user. Desktop entries validated with only the advisory about
using both System and Settings categories, and desktop/icon caches were updated.
No service lifecycle or session configuration changes were made during installation.
