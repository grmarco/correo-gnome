#!/bin/bash
# Prueba de regresión: arranca la demo en una pantalla virtual, recorre unos escenarios
# básicos con clics y teclas, y falla si la app emite algún aviso de GLib/GTK/Adwaita
# (`-CRITICAL`, `-WARNING`) o un pánico de Rust.
#
# Requisitos: Xvfb, dbus-run-session, python3, libX11/libXtst. Compila la app en debug (la demo
# solo existe en debug). Uso: scripts/check-gtk-warnings.sh
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
cargo build -p correo-app >&2 || exit 2
BIN=$ROOT/target/debug/correo-app
ACT=$ROOT/scripts/gtk_actions.py
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
FAIL=0

# run NOMBRE ENV acción...  (ENV: "" o "VAR=valor")
run() {
    local name=$1 extra=$2; shift 2
    local dir=$TMP/$name
    mkdir -p "$dir"/{data,config,state,run}; chmod 700 "$dir/run"
    env GDK_BACKEND=x11 WAYLAND_DISPLAY= GSK_RENDERER=cairo CORREO_DEMO=1 $extra \
        XDG_DATA_HOME="$dir/data" XDG_CONFIG_HOME="$dir/config" \
        XDG_STATE_HOME="$dir/state" XDG_RUNTIME_DIR="$dir/run" RUST_BACKTRACE=1 \
        dbus-run-session -- bash -c '
            "$0" 2>"$1/app.err" & app=$!
            sleep 5
            python3 -I "$2" "${@:3}"
            sleep 1
            kill $app 2>/dev/null; wait $app 2>/dev/null' "$BIN" "$dir" "$ACT" "$@" >/dev/null 2>&1
    # Solo líneas de la propia app (el portal y Mesa meten ruido ajeno).
    if grep -E '\(correo-app:[0-9]+\).*(CRITICAL|WARNING)|panicked at' "$dir/app.err" >"$dir/bad"; then
        echo "FALLO en el escenario '$name':"; sed 's/^/    /' "$dir/bad"; FAIL=1
    else
        echo "ok: $name"
    fi
}

SCREEN=${SCREEN:-1400x900x24}
inner() {
    # Coordenadas para la ventana de demo de 1180x760 (ver gtk_actions.py).
    run principal "" \
        click:28:27 wait:1 key:Escape \
        click:28:27 wait:1 click:590:350 wait:1 key:Escape wait:1 \
        click:340:95 click:340:175 key:Down key:Down key:Up \
        key:Control_L+n wait:1 key:Escape wait:1 \
        key:Control_L+n wait:1 type:hola key:Escape wait:1 key:Escape wait:1 \
        click:318:27 click:340:95 click:340:175 key:Control_L+a key:Escape click:318:27 \
        key:Control_L+a key:Escape \
        click:233:120 click:233:120 click:27:223 click:27:223 click:27:189 click:27:189 \
        rclick:100:120 wait:1 key:Escape \
        click:340:95 click:1037:32 wait:1 key:Escape \
        click:400:27 type:presupuesto key:Return wait:1 key:Escape \
        resize:560:700 wait:1 click:300:100 key:Escape resize:1180:760 wait:1 \
        key:Control_L+n wait:1 key:Escape wait:1
    run lote "" \
        click:318:27 click:340:95 click:340:175 click:890:534 wait:1 \
        click:340:250 click:462:729 wait:1 key:Escape wait:1 \
        click:711:534 wait:2 key:Control_L+a click:1073:534 wait:2 key:Escape \
        click:340:95 key:Delete wait:2 click:340:95 key:Down key:Down key:Return wait:1 \
        click:98:155 wait:1 click:98:555 wait:1 click:90:75 wait:1
    run cuenta-con-cliente CORREO_GOOGLE_CLIENT_ID=prueba.apps.googleusercontent.com \
        click:28:27 wait:1 click:590:350 wait:3 key:Escape wait:1
    # Adjuntos del demo: descarga y cancelación, error, confirmación de un .sh, menú «Responder»,
    # y la barra de acciones compacta.
    run adjuntos "" \
        click:400:490 wait:1 click:887:32 wait:1 key:Escape \
        click:1125:258 wait:2 click:1125:258 wait:1 \
        click:1125:313 wait:3 click:1125:313 wait:3 \
        click:1125:368 wait:1 key:Escape wait:1 \
        click:1125:207 wait:1 click:1125:207 wait:1 \
        resize:560:700 wait:1 click:413:32 wait:1 key:Escape wait:1 resize:1180:760 wait:1
    # Estado de carga de las carpetas: cargando (barra y esqueleto) y con error (reintentar).
    run carga CORREO_DEMO_SYNC=loading click:95:223 wait:1 click:95:155 wait:1 click:28:27 wait:1 key:Escape
    run fallo CORREO_DEMO_SYNC=failed click:95:223 wait:3 click:425:511 wait:3 click:95:155 wait:2
    run vacio CORREO_DEMO_EMPTY=1 \
        click:589:534 wait:1 key:Escape wait:1 click:589:534 wait:1 key:Escape wait:1
}

if [ -z "${DISPLAY:-}" ] || [ -n "${FORCE_XVFB:-}" ]; then
    exec xvfb-run -a -s "-screen 0 $SCREEN" env FORCE_XVFB= "$0" "$@"
fi
inner
[ $FAIL -eq 0 ] && echo "Sin avisos de GTK." || echo "Hay avisos de GTK."
exit $FAIL
