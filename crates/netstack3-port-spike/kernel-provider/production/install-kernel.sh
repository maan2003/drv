#!/usr/bin/env bash
# Current production frontend; disposable Linux 7.3 source only, never host install.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
exec bash "$here/rust-abi5/install-kernel.sh" "${1:?usage: install-kernel.sh DISPOSABLE_LINUX_7_3_SOURCE}"
