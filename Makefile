# Empaquetado de Correo. Uso:
#   make build                    compila el binario optimizado (sin root)
#   sudo make install PREFIX=/usr
#   sudo make uninstall PREFIX=/usr
# Con DESTDIR (empaquetado) no se actualiza la caché de iconos del host.

APP_ID   := io.github.gmarco.Correo
PREFIX   ?= /usr/local
DESTDIR  ?=
CARGO    ?= cargo
BINDIR   := $(DESTDIR)$(PREFIX)/bin
DATADIR  := $(DESTDIR)$(PREFIX)/share
ICONDIR  := $(DATADIR)/icons/hicolor
BIN      := target/release/correo-app

ICON_SVG     := data/icons/hicolor/scalable/apps/$(APP_ID).svg
ICON_SYMBOLIC := data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
# Iconos simbólicos propios de la interfaz (también van incrustados en el binario).
ACTION_ICONS := $(wildcard data/icons/hicolor/scalable/actions/*-symbolic.svg)

.PHONY: all build install uninstall clean

all: build

build:
	$(CARGO) build --release -p correo-app

$(BIN):
	$(CARGO) build --release -p correo-app

install: $(BIN)
	install -Dm755 $(BIN) $(BINDIR)/correo-app
	install -Dm644 data/$(APP_ID).desktop $(DATADIR)/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).metainfo.xml $(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 $(ICON_SVG) $(ICONDIR)/scalable/apps/$(APP_ID).svg
	install -Dm644 $(ICON_SYMBOLIC) $(ICONDIR)/symbolic/apps/$(APP_ID)-symbolic.svg
	for f in $(ACTION_ICONS); do install -Dm644 $$f $(ICONDIR)/scalable/actions/$$(basename $$f); done
	@if [ -z "$(DESTDIR)" ] && command -v gtk-update-icon-cache >/dev/null 2>&1; then \
		gtk-update-icon-cache -q -t -f $(ICONDIR) || true; \
	fi

uninstall:
	rm -f $(BINDIR)/correo-app
	rm -f $(DATADIR)/applications/$(APP_ID).desktop
	rm -f $(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	rm -f $(ICONDIR)/scalable/apps/$(APP_ID).svg
	rm -f $(ICONDIR)/symbolic/apps/$(APP_ID)-symbolic.svg
	for f in $(ACTION_ICONS); do rm -f $(ICONDIR)/scalable/actions/$$(basename $$f); done
	@if [ -z "$(DESTDIR)" ] && command -v gtk-update-icon-cache >/dev/null 2>&1; then \
		gtk-update-icon-cache -q -t -f $(ICONDIR) || true; \
	fi

clean:
	$(CARGO) clean
