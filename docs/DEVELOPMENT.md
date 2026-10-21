# Desarrollo

## Avisos de GTK

La interfaz no debe emitir ningún `Gtk-CRITICAL`, `Gtk-WARNING` ni `Adwaita-WARNING`. Para comprobarlo
(arranca la demo en Xvfb, aislada con `dbus-run-session` y XDG temporales, y recorre escenarios con
clics y teclas): `scripts/check-gtk-warnings.sh` (requiere `Xvfb`, `dbus-run-session` y `python3`;
no forma parte de `cargo test` porque necesita display).
