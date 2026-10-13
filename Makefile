PREFIX ?= /usr
DESTDIR ?=

.PHONY: build install check
build:
	cargo build --release --locked

check:
	cargo fmt --check
	cargo test --locked
	cargo clippy --all-targets --locked -- -D warnings

install:
	install -Dm755 target/release/pilot $(DESTDIR)$(PREFIX)/bin/pilot
	install -Dm755 target/release/pilot-helper $(DESTDIR)/usr/libexec/pilot-helper
	install -Dm644 assets/org.voidlab.Pilot.desktop $(DESTDIR)$(PREFIX)/share/applications/org.voidlab.Pilot.desktop
	install -Dm644 assets/org.voidlab.Pilot.svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/org.voidlab.Pilot.svg
	install -Dm644 assets/org.voidlab.Pilot.policy $(DESTDIR)/usr/share/polkit-1/actions/org.voidlab.Pilot.policy
	install -dm755 $(DESTDIR)/usr/lib/dinit.d/user/boot.d
