APP_NAME     := dbased
APP_ID       := dev.tsndr.dbased
CARGO        ?= cargo

PREFIX       ?= /usr/local
LOCAL_PREFIX ?= $(HOME)/.local
BINDIR       := $(PREFIX)/bin
DATADIR      := $(PREFIX)/share
APPDIR       := $(DATADIR)/applications
ICONDIR      := $(DATADIR)/icons/hicolor/scalable/apps
METAINFODIR  := $(DATADIR)/metainfo

.PHONY: all build install install-files uninstall install-user uninstall-user arch-pkg install-arch clean

all: build

build:
	$(CARGO) build --release --locked

install-files:
	install -Dm755 target/release/$(APP_NAME) "$(DESTDIR)$(BINDIR)/$(APP_NAME)"
	install -Dm644 data/$(APP_ID).desktop "$(DESTDIR)$(APPDIR)/$(APP_ID).desktop"
	install -Dm644 data/$(APP_ID).svg "$(DESTDIR)$(ICONDIR)/$(APP_ID).svg"
	install -Dm644 data/$(APP_ID).metainfo.xml "$(DESTDIR)$(METAINFODIR)/$(APP_ID).metainfo.xml"

install: build install-files
	@if [ -z "$(DESTDIR)" ]; then \
		gtk-update-icon-cache -q -t -f "$(DATADIR)/icons/hicolor" 2>/dev/null || true; \
		update-desktop-database -q "$(APPDIR)" 2>/dev/null || true; \
	fi

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/$(APP_NAME)"
	rm -f "$(DESTDIR)$(APPDIR)/$(APP_ID).desktop"
	rm -f "$(DESTDIR)$(ICONDIR)/$(APP_ID).svg"
	rm -f "$(DESTDIR)$(METAINFODIR)/$(APP_ID).metainfo.xml"
	@if [ -z "$(DESTDIR)" ]; then \
		gtk-update-icon-cache -q -t -f "$(DATADIR)/icons/hicolor" 2>/dev/null || true; \
		update-desktop-database -q "$(APPDIR)" 2>/dev/null || true; \
	fi

install-user: build
	$(MAKE) PREFIX="$(LOCAL_PREFIX)" install-files
	@gtk-update-icon-cache -q -t -f "$(LOCAL_PREFIX)/share/icons/hicolor" 2>/dev/null || true
	@update-desktop-database -q "$(LOCAL_PREFIX)/share/applications" 2>/dev/null || true

uninstall-user:
	$(MAKE) PREFIX="$(LOCAL_PREFIX)" uninstall

arch-pkg:
	makepkg -sf

install-arch:
	makepkg -sfi

clean:
	$(CARGO) clean
