#!/usr/bin/env bash
# Arranca Claude Remote Control en esta carpeta; aparece en claude.ai/code como entorno "correo".
cd "$(dirname "$0")" || exit 1
exec claude remote-control --name "correo" "$@"
