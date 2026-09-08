#!/usr/bin/env bash
# Ask the active userspace driver to stop. Its signal guard unwinds VFIO; the
# watchdog-covered wrapper then restores and verifies the native ajay link.
set -euo pipefail
sudo -n pkill -TERM -f '/libexec/mt7921-full-firmware-validation --run-one-shot-sae-auth'
