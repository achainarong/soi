# soi — build, run and install as a desktop app.
#
#   make            build an optimized binary (target/release/soi)
#   make run        build and start it
#   make dev        start a quick-to-build debug version next to the installed one
#   make test       run the tests
#   make install    install for this user: binary, app menu entry, icon and
#                   the Claude Code / OpenCode integrations (AGENTS=0 skips them)
#   make uninstall  remove all of that again
#   make appimage   Linux: build target/soi-<arch>.AppImage, with GTK inside
#   make bundle     macOS: build target/soi.app, with GTK inside
#
# Installs into ~/.local by default; `make install PREFIX=/usr/local` (with
# sudo) installs system-wide instead. On macOS, soi.app goes to
# ~/Applications (APPDIR) and $(BINDIR)/soi links to it, for shells.

APP_ID  := dev.soi.Soi
PREFIX  ?= $(HOME)/.local
BINDIR  := $(PREFIX)/bin
DATADIR := $(PREFIX)/share
APPSDIR := $(DATADIR)/applications
ICONDIR := $(DATADIR)/icons/hicolor/scalable/apps

BINARY  := target/release/soi
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APPDIR  ?= $(HOME)/Applications
BUNDLE  := $(APPDIR)/soi.app
# Install the agent integrations (Claude Code hooks, OpenCode plugin)?
AGENTS  ?= 1

.PHONY: all build run dev test test-gui bundle appimage install uninstall clean

all: build

build:
	cargo build --release

run: build
	$(BINARY)

# Runs next to an installed soi, with its own session file.
dev:
	SOI_DEV=1 cargo run

test:
	cargo test

# AppKit requires GTK on the process's main thread, not a libtest worker.
test-gui:
	cargo test --features gui-tests --test gui

ifeq ($(shell uname),Darwin)

bundle: build
	python3 scripts/macos_bundle.py --binary "$(BINARY)" --output target/soi.app --version "$(VERSION)"

install: build
	python3 scripts/macos_bundle.py --binary "$(BINARY)" --output "$(BUNDLE)" --version "$(VERSION)"
	mkdir -p "$(BINDIR)"
	ln -sf "$(BUNDLE)/Contents/MacOS/soi" "$(BINDIR)/soi"
	@# Tell agents set up for this user about soi; not for root (sudo).
	@if [ "$(AGENTS)" != 0 ] && [ "$$(id -u)" != 0 ]; then \
		"$(BINDIR)/soi" install-agents || true; \
	fi
	@echo "Installed. Start soi from Launchpad or $(APPDIR), or run: $(BINDIR)/soi"

uninstall:
	rm -rf "$(BUNDLE)"
	rm -f "$(BINDIR)/soi"
	@echo "Uninstalled. Settings and layouts stay in ~/.config/soi and ~/.local/state/soi."

else

appimage: build
	scripts/linux_appimage.sh "$(BINARY)" "target/soi-$(shell uname -m).AppImage"

install: build
	install -Dm755 $(BINARY) $(BINDIR)/soi
	install -Dm644 data/$(APP_ID).svg $(ICONDIR)/$(APP_ID).svg
	install -d $(APPSDIR)
	@# The icon by full path: launchers that cached "no such icon" (KDE does)
	@# still show it, and it doesn't depend on an icon theme lookup.
	sed -e 's|@BINDIR@|$(BINDIR)|g' -e 's|@ICON@|$(ICONDIR)/$(APP_ID).svg|g' \
		data/$(APP_ID).desktop.in > $(APPSDIR)/$(APP_ID).desktop
	chmod 644 $(APPSDIR)/$(APP_ID).desktop
	@# Let the desktop pick up the new entry and icon (fine if these are missing).
	-update-desktop-database -q $(APPSDIR) 2>/dev/null
	@# Only refresh an existing icon cache; creating one could hide other icons.
	@if [ -f $(DATADIR)/icons/hicolor/icon-theme.cache ]; then \
		gtk-update-icon-cache -q $(DATADIR)/icons/hicolor 2>/dev/null || true; \
	fi
	-kbuildsycoca6 --noincremental >/dev/null 2>&1
	@# Tell agents set up for this user about soi; not for root (sudo).
	@if [ "$(AGENTS)" != 0 ] && [ "$$(id -u)" != 0 ]; then \
		$(BINDIR)/soi install-agents || true; \
	fi
	@echo "Installed. Start soi from your app menu, or run: $(BINDIR)/soi"

uninstall:
	rm -f $(BINDIR)/soi $(APPSDIR)/$(APP_ID).desktop $(ICONDIR)/$(APP_ID).svg
	-update-desktop-database -q $(APPSDIR) 2>/dev/null
	-kbuildsycoca6 --noincremental >/dev/null 2>&1
	@echo "Uninstalled. Settings and layouts stay in ~/.config/soi and ~/.local/state/soi."

endif

clean:
	cargo clean
